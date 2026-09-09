// SPDX-License-Identifier: GPL-2.0-only

use fidl_fuchsia_wlan_common as common;
use fidl_fuchsia_wlan_driver as driver;
use fidl_fuchsia_wlan_ieee80211 as ieee;
use fidl_fuchsia_wlan_internal as internal;
use fidl_fuchsia_wlan_mlme as mlme;
use fidl_fuchsia_wlan_sme as sme;
use fidl_fuchsia_wlan_softmac as softmac;
use std::collections::VecDeque;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::sync::{Arc, Mutex};
use wifi_control_service::{PreparedServer, UnixSeqpacketEndpoint};
use wifi_supervisor_wire::{LifecycleKind, LifecycleMessage};
use wlan_control_wire::{ConnectReply, Message, Packet};
use wlan_softmac_host::runtime::ClientRuntime;
use wlan_softmac_host::{
    ClientRuntimeDriver, WlanSoftmac, WlanSoftmacLifecycle, WlanSoftmacUpcalls,
};

const GENERATION: [u8; 16] = [0x6d; 16];
const CLIENT: [u8; 6] = [2, 0, 0, 0, 0, 1];
const AP: [u8; 6] = [2, 0, 0, 0, 0, 2];

#[derive(Default)]
struct Effects {
    upcalls: Option<Box<dyn WlanSoftmacUpcalls>>,
    pending_rx: VecDeque<Vec<u8>>,
    links: Vec<bool>,
}

#[derive(Clone)]
struct FakeSoftmac(Arc<Mutex<Effects>>);

impl WlanSoftmacLifecycle for FakeSoftmac {
    fn start(&mut self, upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status> {
        self.0.lock().unwrap().upcalls = Some(upcalls);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), zx::Status> {
        self.0.lock().unwrap().upcalls = None;
        Ok(())
    }
}

impl ClientRuntimeDriver for FakeSoftmac {
    fn drive(&mut self) -> Result<bool, zx::Status> {
        let mut effects = self.0.lock().unwrap();
        let Some(frame) = effects.pending_rx.pop_front() else {
            return Ok(false);
        };
        effects.upcalls.as_mut().unwrap().recv(frame, rx_info());
        Ok(true)
    }

    fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status> {
        self.0.lock().unwrap().links.push(up);
        Ok(())
    }

    fn reset(&mut self) -> Result<(), zx::Status> {
        self.stop()
    }
}

impl WlanSoftmac for FakeSoftmac {
    fn query(&mut self) -> Result<softmac::WlanSoftmacQueryResponse, zx::Status> {
        Ok(softmac::WlanSoftmacQueryResponse {
            sta_addr: Some(CLIENT),
            factory_addr: Some(CLIENT),
            mac_role: Some(common::WlanMacRole::Client),
            hardware_capability: Some(0),
            band_caps: Some(vec![softmac::WlanSoftmacBandCapability {
                band: Some(ieee::WlanBand::TwoGhz),
                basic_rates: Some(vec![0x82, 0x84]),
                primary_channels: Some(vec![channel()]),
                ..Default::default()
            }]),
            ..Default::default()
        })
    }
    fn query_discovery_support(&mut self) -> Result<softmac::DiscoverySupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_mac_sublayer_support(&mut self) -> Result<common::MacSublayerSupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_security_support(&mut self) -> Result<common::SecuritySupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_spectrum_management_support(
        &mut self,
    ) -> Result<common::SpectrumManagementSupport, zx::Status> {
        Ok(Default::default())
    }
    fn set_channel(
        &mut self,
        _: softmac::WlanSoftmacBaseSetChannelRequest,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn join_bss(&mut self, _: driver::JoinBssRequest) -> Result<(), zx::Status> {
        Ok(())
    }
    fn install_key(&mut self, _: softmac::WlanKeyConfiguration) -> Result<(), zx::Status> {
        Ok(())
    }
    fn notify_association_complete(
        &mut self,
        _: softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn clear_association(
        &mut self,
        _: softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn start_passive_scan(
        &mut self,
        _: softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn start_active_scan(
        &mut self,
        _: softmac::WlanSoftmacStartActiveScanRequest,
    ) -> Result<softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn cancel_scan(
        &mut self,
        _: softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn update_wmm_parameters(
        &mut self,
        _: softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn queue_tx(&mut self, bytes: &[u8], _: softmac::WlanTxInfoFlags) -> Result<(), zx::Status> {
        let response = match bytes.first().copied() {
            Some(0xb0) => authentication_response(),
            Some(0x00) => association_response(),
            Some(0xc0) => return Ok(()),
            _ => return Err(zx::Status::NOT_SUPPORTED),
        };
        self.0.lock().unwrap().pending_rx.push_back(response);
        Ok(())
    }
}

#[test]
fn generic_runtime_publishes_one_generation_and_disconnect_revokes_it() {
    let effects = Arc::new(Mutex::new(Effects::default()));
    let runtime = futures::executor::block_on(ClientRuntime::new(
        FakeSoftmac(effects.clone()),
        Default::default(),
        device_info(),
        Default::default(),
        Default::default(),
        fuchsia_inspect::Inspector::default(),
    ))
    .unwrap();
    let (policy_server, policy_peer) = pair();
    let (supervisor_server, supervisor_peer) = pair();
    let policy = UnixSeqpacketEndpoint::from_inherited_fd(policy_peer).unwrap();
    let mut server = PreparedServer::new(policy_server, supervisor_server, GENERATION, runtime)
        .unwrap()
        .post_lockdown_open_complete()
        .unwrap();

    drive_until(&mut server, || {
        policy.try_receive_packet().unwrap().is_some()
    });
    send(&policy, 1, Message::Connect(connect_request()));
    let mut connected = false;
    let mut ethernet = None;
    for _ in 0..2_000 {
        futures::executor::block_on(server.drive_once()).unwrap();
        while let Some(packet) = policy.try_receive_packet().unwrap() {
            if matches!(
                packet.packet.message,
                Message::ConnectReply(wlan_control_wire::Reply {
                    result: ConnectReply::Completed(sme::ConnectResult {
                        code: ieee::StatusCode::Success,
                        ..
                    }),
                    ..
                })
            ) {
                connected = true;
            }
        }
        if ethernet.is_none() {
            ethernet = try_receive_supervisor(&supervisor_peer);
        }
        if connected && ethernet.is_some() {
            break;
        }
    }
    assert!(connected, "pinned runtime did not complete connection");
    let (record, mut fds) = ethernet.expect("Ethernet generation was not published");
    assert_eq!(
        LifecycleMessage::decode(&record).unwrap(),
        LifecycleMessage {
            kind: LifecycleKind::Install,
            wifi_generation: GENERATION,
            ethernet_generation: 1,
            mac_address: CLIENT
        }
    );
    assert_eq!(fds.len(), 1);
    let frame = fds.pop().unwrap();
    assert_eq!(effects.lock().unwrap().links.as_slice(), &[true]);
    for _ in 0..8 {
        futures::executor::block_on(server.drive_once()).unwrap();
        assert!(
            try_receive_supervisor(&supervisor_peer).is_none(),
            "Ethernet capability published twice"
        );
    }

    send(
        &policy,
        2,
        Message::Disconnect(sme::UserDisconnectReason::FidlStopClientConnectionsRequest),
    );
    let mut disconnected = false;
    for _ in 0..2_000 {
        futures::executor::block_on(server.drive_once()).unwrap();
        while let Some(packet) = policy.try_receive_packet().unwrap() {
            if matches!(packet.packet.message, Message::DisconnectReply(_)) {
                disconnected = true;
            }
        }
        if disconnected && poll_hup(frame.as_raw_fd()) {
            break;
        }
    }
    assert!(disconnected, "disconnect reply was not delivered");
    assert!(
        poll_hup(frame.as_raw_fd()),
        "published Ethernet generation stayed live"
    );
    assert!(effects.lock().unwrap().links.ends_with(&[false]));

    send(&policy, 3, Message::Connect(connect_request()));
    let mut reconnected = false;
    let mut replacement = None;
    for _ in 0..2_000 {
        futures::executor::block_on(server.drive_once()).unwrap();
        while let Some(packet) = policy.try_receive_packet().unwrap() {
            if matches!(packet.packet.message, Message::ConnectReply(wlan_control_wire::Reply { result: ConnectReply::Completed(sme::ConnectResult { code: ieee::StatusCode::Success, .. }), .. })) {
                reconnected = true;
            }
        }
        if replacement.is_none() {
            replacement = try_receive_supervisor(&supervisor_peer);
        }
        if reconnected && replacement.is_some() {
            break;
        }
    }
    assert!(reconnected, "pinned runtime did not reconnect");
    let (record, fds) = replacement.expect("replacement Ethernet generation was not published");
    assert_eq!(
        LifecycleMessage::decode(&record).unwrap(),
        LifecycleMessage {
            kind: LifecycleKind::Install,
            wifi_generation: GENERATION,
            ethernet_generation: 2,
            mac_address: CLIENT
        }
    );
    assert_eq!(fds.len(), 1);
}

fn drive_until<R: wifi_control_service::WifiRuntime>(
    server: &mut wifi_control_service::ControlServer<R>,
    mut done: impl FnMut() -> bool,
) {
    for _ in 0..2_000 {
        futures::executor::block_on(server.drive_once()).unwrap();
        if done() {
            return;
        }
    }
    panic!("service did not make bounded progress");
}

fn send(endpoint: &UnixSeqpacketEndpoint, request_id: u64, message: Message) {
    assert!(
        endpoint
            .try_send_packet(
                &Packet {
                    generation: GENERATION,
                    request_id,
                    message
                },
                None
            )
            .unwrap()
    );
}

fn pair() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1; 2];
    assert_eq!(
        unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        },
        0
    );
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

fn try_receive_supervisor(fd: &OwnedFd) -> Option<([u8; 40], Vec<OwnedFd>)> {
    let mut bytes = [0u8; 40];
    let mut control = [0usize; 4];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = &mut iov;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = std::mem::size_of_val(&control);
    let received = unsafe {
        libc::recvmsg(
            fd.as_raw_fd(),
            &mut header,
            libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
        )
    };
    if received < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return None;
        }
        panic!("supervisor recvmsg: {error}");
    }
    assert_eq!(received, 40);
    assert_eq!(header.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC), 0);
    let mut fds = Vec::new();
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&header);
        while !cmsg.is_null() {
            assert_eq!((*cmsg).cmsg_level, libc::SOL_SOCKET);
            assert_eq!((*cmsg).cmsg_type, libc::SCM_RIGHTS);
            let count =
                ((*cmsg).cmsg_len - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<i32>();
            let data = libc::CMSG_DATA(cmsg).cast::<i32>();
            for index in 0..count {
                fds.push(OwnedFd::from_raw_fd(*data.add(index)));
            }
            cmsg = libc::CMSG_NXTHDR(&header, cmsg);
        }
    }
    Some((bytes, fds))
}

fn channel() -> ieee::ChannelNumber {
    ieee::ChannelNumber {
        band: ieee::WlanBand::TwoGhz,
        number: 1,
    }
}
fn device_info() -> mlme::DeviceInfo {
    mlme::DeviceInfo {
        sta_addr: CLIENT,
        factory_addr: CLIENT,
        role: common::WlanMacRole::Client,
        bands: vec![mlme::BandCapability {
            band: ieee::WlanBand::TwoGhz,
            basic_rates: vec![0x82, 0x84],
            ht_cap: None,
            vht_cap: None,
            primary_channels: vec![channel()],
        }],
        softmac_hardware_capability: 0,
        qos_capable: false,
    }
}
fn connect_request() -> sme::ConnectRequest {
    sme::ConnectRequest {
        ssid: b"test".to_vec(),
        bss_description: ieee::BssDescription {
            bssid: AP,
            bss_type: ieee::BssType::Infrastructure,
            beacon_period: 100,
            capability_info: 1,
            ies: vec![0, 4, b't', b'e', b's', b't', 1, 2, 0x82, 0x84],
            primary: channel(),
            bandwidth: ieee::ChannelBandwidth::Cbw20,
            vht_secondary_80_channel: channel(),
            rssi_dbm: -30,
            snr_db: 20,
        },
        multiple_bss_candidates: false,
        authentication: internal::Authentication {
            protocol: internal::Protocol::Open,
            credentials: None,
        },
        deprecated_scan_type: common::ScanType::Passive,
    }
}
fn rx_info() -> softmac::WlanRxInfo {
    softmac::WlanRxInfo {
        rx_flags: softmac::WlanRxInfoFlags::empty(),
        valid_fields: softmac::WlanRxInfoValid::empty(),
        phy: ieee::WlanPhyType::Dsss,
        data_rate: 0,
        primary: channel(),
        bandwidth: ieee::ChannelBandwidth::Cbw20,
        vht_secondary_80_channel: channel(),
        mcs: 0,
        rssi_dbm: 0,
        snr_dbh: 0,
    }
}
fn authentication_response() -> Vec<u8> {
    let mut b = vec![0xb0, 0, 0, 0];
    b.extend_from_slice(&CLIENT);
    b.extend_from_slice(&AP);
    b.extend_from_slice(&AP);
    b.extend_from_slice(&[0, 0, 0, 0, 2, 0, 0, 0]);
    b
}
fn association_response() -> Vec<u8> {
    let mut b = vec![0x10, 0, 0, 0];
    b.extend_from_slice(&CLIENT);
    b.extend_from_slice(&AP);
    b.extend_from_slice(&AP);
    b.extend_from_slice(&[0, 0, 1, 0, 0, 0, 42, 0, 1, 2, 0x82, 0x84]);
    b
}
fn poll_hup(fd: i32) -> bool {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    assert!(unsafe { libc::poll(&mut p, 1, 0) } >= 0);
    p.revents & libc::POLLHUP != 0
}
