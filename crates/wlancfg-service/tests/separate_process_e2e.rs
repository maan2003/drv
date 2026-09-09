// SPDX-License-Identifier: GPL-2.0-only

//! Deterministic separate-process fixture.
//!
//! The `wlancfg-fixture-no-kernel-sandbox` role deliberately bypasses only the
//! kernel namespace/seccomp operations, which are unavailable in some build
//! sandboxes. It executes the same production policy function and transport.
//! Production `wlancfg-service` has no such mode and always fails closed.

use fidl_fuchsia_wlan_ieee80211 as ieee;
use fidl_fuchsia_wlan_internal as internal;
use fidl_fuchsia_wlan_sme as sme;
use futures::channel::mpsc;
use std::{
    fs::{File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::process::CommandExt,
    },
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use wifi_control_service::{PreparedServer, RuntimeError, WifiRuntime};
use wlancfg_selection::{
    client::types,
    config_management::{
        Credential, NetworkIdentifier, SavedNetworksManager, SavedNetworksManagerApi, SecurityType,
    },
    telemetry::{TelemetryEvent, TelemetrySender},
};

const GENERATION: [u8; 16] = [0x71; 16];
const SSID: &[u8] = b"selected-network";
const PASSWORD: &[u8] = b"selected-password";
const BSSID: [u8; 6] = [2, 7, 1, 2, 3, 4];

fn main() {
    let result = match std::env::var("DRV_WLANCFG_E2E_ROLE").as_deref() {
        Ok("wifi") => wifi_child(),
        Ok("wlancfg-fixture-no-kernel-sandbox") => wlancfg_child(),
        _ => parent(),
    };
    if let Err(error) = result {
        eprintln!("separate-process-e2e: {error:#}");
        std::process::exit(1);
    }
}

fn parent() -> anyhow::Result<()> {
    let state = TestDirectory::new()?;
    seed(&state)?;

    let first = run_scenario(&state, "retry-success")?;
    assert_eq!(first.scans.len(), 2, "{first:?}");
    assert!(first.scans[0].contains("passive"), "{first:?}");
    assert!(
        first.scans[1].contains("active ssid_match=1 channel_match=1"),
        "{first:?}"
    );
    assert_eq!(first.connects.len(), 4, "{first:?}");
    assert!(
        first
            .connects
            .iter()
            .all(|line| line.contains("bssid_match=1 credential_match=1"))
    );
    assert!(
        first
            .connects
            .iter()
            .all(|line| line.contains("active_result_match=1"))
    );
    let times: Vec<u64> = first
        .connects
        .iter()
        .map(|line| field(line, "ms"))
        .collect();
    let delays: Vec<u64> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
    for (actual, expected) in delays.iter().zip([400u64, 800, 1200]) {
        assert!(actual.abs_diff(expected) < 180, "attempt times {times:?}");
    }
    assert!(first.wifi.contains("SIGNAL_EVENT_IMMEDIATE"));
    assert!(
        !first
            .wifi
            .as_bytes()
            .windows(PASSWORD.len())
            .any(|bytes| bytes == PASSWORD)
    );
    assert!(load_selected(&state)?.has_ever_connected);

    // No reseeding: a fresh policy process must select the credential loaded
    // from the same directory capability.
    let restarted = run_scenario(&state, "success")?;
    assert_eq!(restarted.connects.len(), 1, "{restarted:?}");
    assert!(restarted.connects[0].contains("credential_match=1"));

    let rejected = run_scenario(&state, "credential-rejected")?;
    assert_eq!(rejected.connects.len(), 1, "{rejected:?}");
    let terminal = run_scenario(&state, "generation-terminal")?;
    assert_eq!(terminal.connects.len(), 1, "{terminal:?}");
    Ok(())
}

#[derive(Debug)]
struct ScenarioOutput {
    wifi: String,
    scans: Vec<String>,
    connects: Vec<String>,
}

fn run_scenario(state: &TestDirectory, scenario: &str) -> anyhow::Result<ScenarioOutput> {
    let (policy_parent, policy_wifi) = sockets()?;
    let (supervisor_parent, supervisor_wifi) = sockets()?;
    let state_fd = state.capability()?;
    let wifi = spawn_role(
        "wifi",
        scenario,
        policy_wifi.as_raw_fd(),
        supervisor_wifi.as_raw_fd(),
    )?;
    let policy = spawn_role(
        "wlancfg-fixture-no-kernel-sandbox",
        scenario,
        policy_parent.as_raw_fd(),
        state_fd.as_raw_fd(),
    )?;
    assert_ne!(wifi.id(), policy.id());
    assert_ne!(wifi.id(), std::process::id());
    assert_ne!(policy.id(), std::process::id());
    inspect_stopped_child(&wifi, 3, 4)?;
    inspect_stopped_child(&policy, 3, 4)?;
    let policy_state_link = std::fs::read_link(format!("/proc/{}/fd/4", policy.id()))?;
    assert!(
        policy_state_link
            .to_string_lossy()
            .contains(state.path().to_string_lossy().as_ref())
    );
    let wifi_lifecycle_link = std::fs::read_link(format!("/proc/{}/fd/4", wifi.id()))?;
    assert!(wifi_lifecycle_link.to_string_lossy().contains("socket:"));

    // Only the parent/supervisor retains the other lifecycle endpoint.
    drop(policy_parent);
    drop(policy_wifi);
    drop(supervisor_wifi);
    drop(state_fd);
    let policy_output = wait_with_deadline(policy)?;
    let wifi_output = wait_with_deadline(wifi)?;
    drop(supervisor_parent);
    assert!(
        policy_output.status.success(),
        "policy: {}",
        String::from_utf8_lossy(&policy_output.stderr)
    );
    assert!(
        wifi_output.status.success(),
        "wifi: {}",
        String::from_utf8_lossy(&wifi_output.stderr)
    );
    let wifi = String::from_utf8(wifi_output.stdout)?;
    Ok(ScenarioOutput {
        scans: wifi
            .lines()
            .filter(|line| line.starts_with("SCAN "))
            .map(str::to_owned)
            .collect(),
        connects: wifi
            .lines()
            .filter(|line| line.starts_with("CONNECT "))
            .map(str::to_owned)
            .collect(),
        wifi,
    })
}

fn spawn_role(
    role: &str,
    scenario: &str,
    fd3: RawFd,
    fd4: RawFd,
) -> io::Result<std::process::Child> {
    let fd3 = duplicate_high(fd3)?;
    let fd4 = duplicate_high(fd4)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .env("DRV_WLANCFG_E2E_ROLE", role)
        .env("DRV_WLANCFG_E2E_SCENARIO", scenario)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(fd3.as_raw_fd(), 3) < 0 || libc::dup2(fd4.as_raw_fd(), 4) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}

fn duplicate_high(fd: RawFd) -> io::Result<OwnedFd> {
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
    if copy < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(copy) })
    }
}

fn inspect_stopped_child(
    child: &std::process::Child,
    first: RawFd,
    second: RawFd,
) -> anyhow::Result<()> {
    let pid = child.id();
    let mut status = 0;
    let waited = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WUNTRACED) };
    if waited != pid as libc::pid_t || !libc::WIFSTOPPED(status) {
        anyhow::bail!("fixture child did not stop at capability checkpoint");
    }
    let mut fds: Vec<_> = std::fs::read_dir(format!("/proc/{pid}/fd"))?
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .parse::<RawFd>()
                .unwrap()
        })
        .collect();
    fds.sort_unstable();
    assert_eq!(fds, vec![0, 1, 2, first, second]);
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGCONT) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn wait_with_deadline(mut child: std::process::Child) -> anyhow::Result<std::process::Output> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            anyhow::bail!(
                "fixture child timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn sockets() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn wlancfg_child() -> anyhow::Result<()> {
    capability_checkpoint()?;
    let control = unsafe { OwnedFd::from_raw_fd(3) };
    let state = unsafe { OwnedFd::from_raw_fd(4) };
    let prepared =
        wlancfg_service::PreparedHostControlClient::from_inherited_socket(control, GENERATION)?;
    // TEST FIXTURE ONLY: kernel namespace enforcement is not claimed here.
    wlancfg_service::policy::serve_one_generation(prepared, state)
}

fn wifi_child() -> anyhow::Result<()> {
    capability_checkpoint()?;
    let policy = unsafe { OwnedFd::from_raw_fd(3) };
    let supervisor = unsafe { OwnedFd::from_raw_fd(4) };
    let scenario = std::env::var("DRV_WLANCFG_E2E_SCENARIO")?;
    let runtime = FixtureWifi::new(scenario);
    PreparedServer::new(policy, supervisor, GENERATION, runtime)?
        .post_lockdown_open_complete()?
        .run()?;
    Ok(())
}

fn capability_checkpoint() -> io::Result<()> {
    if unsafe { libc::raise(libc::SIGSTOP) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

struct FixtureWifi {
    scenario: String,
    scans: usize,
    attempts: usize,
    pending_scan: Option<sme::ScanRequest>,
    pending_connect: Option<sme::ConnectRequest>,
    events: std::collections::VecDeque<sme::ConnectTransactionEvent>,
    started: Instant,
    connected_idle: usize,
}

impl FixtureWifi {
    fn new(scenario: String) -> Self {
        Self {
            scenario,
            scans: 0,
            attempts: 0,
            pending_scan: None,
            pending_connect: None,
            events: Default::default(),
            started: Instant::now(),
            connected_idle: 0,
        }
    }
}

impl WifiRuntime for FixtureWifi {
    fn public_mac(&self) -> [u8; 6] {
        [2, 0, 0, 0, 0, 1]
    }
    fn take_ethernet_device(&mut self) -> Option<OwnedFd> {
        None
    }
    fn begin_connect(
        &mut self,
        request: sme::ConnectRequest,
        _: Instant,
    ) -> Result<(), RuntimeError> {
        self.pending_connect = Some(request);
        Ok(())
    }
    async fn drive_connect_once(&mut self) -> Result<Option<sme::ConnectResult>, RuntimeError> {
        let Some(request) = self.pending_connect.take() else {
            return Ok(None);
        };
        self.attempts += 1;
        let credential_match = request.authentication.credentials.as_deref()
            == Some(&internal::Credentials::Wpa(
                internal::WpaCredentials::Passphrase(PASSWORD.to_vec()),
            ));
        println!(
            "CONNECT attempt={} ms={} bssid_match={} credential_match={} active_result_match={}",
            self.attempts,
            self.started.elapsed().as_millis(),
            u8::from(request.bss_description.bssid == BSSID),
            u8::from(credential_match),
            u8::from(request.bss_description.rssi_dbm == -35)
        );
        if self.scenario == "generation-terminal" {
            return Err(RuntimeError::DriverFault);
        }
        let fail = self.scenario == "credential-rejected"
            || (self.scenario == "retry-success" && self.attempts < 4);
        if fail {
            return Err(RuntimeError::Failed(sme::ConnectResult {
                code: ieee::StatusCode::RefusedReasonUnspecified,
                is_credential_rejected: self.scenario == "credential-rejected",
                is_reconnect: false,
            }));
        }
        self.events
            .push_back(sme::ConnectTransactionEvent::OnSignalReport {
                ind: internal::SignalReportIndication {
                    rssi_dbm: -39,
                    snr_db: 27,
                },
            });
        println!("SIGNAL_EVENT_IMMEDIATE");
        self.connected_idle = 1;
        Ok(Some(sme::ConnectResult {
            code: ieee::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        }))
    }
    async fn cancel_connect(
        &mut self,
        _: sme::UserDisconnectReason,
        _: Instant,
    ) -> Result<sme::ConnectResult, RuntimeError> {
        unreachable!()
    }
    fn roam(&mut self, _: sme::RoamRequest) -> Result<(), RuntimeError> {
        Ok(())
    }
    fn begin_scan(&mut self, request: sme::ScanRequest, _: Instant) -> Result<(), RuntimeError> {
        self.pending_scan = Some(request);
        Ok(())
    }
    async fn drive_scan_once(
        &mut self,
    ) -> Result<Option<Result<Vec<sme::ScanResult>, sme::ScanErrorCode>>, RuntimeError> {
        let Some(request) = self.pending_scan.take() else {
            return Ok(None);
        };
        self.scans += 1;
        match &request {
            sme::ScanRequest::Passive(_) => println!("SCAN passive"),
            sme::ScanRequest::Active(active) => println!(
                "SCAN active ssid_match={} channel_match={}",
                u8::from(active.ssids == vec![SSID.to_vec()]),
                u8::from(active.channels == vec![6])
            ),
        }
        let active = matches!(request, sme::ScanRequest::Active(_));
        let mut results = vec![scan_result(SSID, BSSID, 6, if active { -35 } else { -42 })];
        if !active {
            results.push(malformed_scan_result());
            results.push(open_scan_result(
                b"decoy-network",
                [2, 9, 9, 9, 9, 9],
                11,
                -80,
            ));
        }
        Ok(Some(Ok(results)))
    }
    async fn drive_once(&mut self) -> Result<bool, RuntimeError> {
        if self.connected_idle > 0 {
            self.connected_idle += 1;
            if self.connected_idle > 100 {
                return Err(RuntimeError::DriverFault);
            }
        }
        Ok(false)
    }
    fn next_connection_event(
        &mut self,
    ) -> Result<Option<sme::ConnectTransactionEvent>, RuntimeError> {
        Ok(self.events.pop_front())
    }
    async fn disconnect(
        &mut self,
        _: sme::UserDisconnectReason,
        _: Instant,
    ) -> Result<(), RuntimeError> {
        Ok(())
    }
}

fn scan_result(ssid: &[u8], bssid: [u8; 6], channel: u8, rssi: i8) -> sme::ScanResult {
    let mut ies = vec![0, ssid.len() as u8];
    ies.extend_from_slice(ssid);
    ies.extend_from_slice(&[1, 4, 2, 4, 11, 22]);
    ies.extend_from_slice(&[
        48, 20, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 8, 204, 0,
    ]);
    ies.extend_from_slice(&[244, 1, 0x20]);
    sme::ScanResult {
        compatibility: sme::Compatibility::Compatible(sme::Compatible {
            mutual_security_protocols: vec![internal::Protocol::Wpa3Personal],
        }),
        timestamp_nanos: 1,
        bss_description: ieee::BssDescription {
            bssid,
            bss_type: ieee::BssType::Infrastructure,
            beacon_period: 100,
            capability_info: 0x11,
            ies,
            primary: ieee::ChannelNumber {
                band: ieee::WlanBand::TwoGhz,
                number: channel,
            },
            bandwidth: ieee::ChannelBandwidth::Cbw20,
            vht_secondary_80_channel: ieee::ChannelNumber {
                band: ieee::WlanBand::TwoGhz,
                number: 0,
            },
            rssi_dbm: rssi,
            snr_db: 30,
        },
    }
}

fn malformed_scan_result() -> sme::ScanResult {
    let mut result = open_scan_result(b"malformed", [2, 8, 8, 8, 8, 8], 1, -10);
    result.bss_description.primary.number = 0;
    result
}

fn open_scan_result(ssid: &[u8], bssid: [u8; 6], channel: u8, rssi: i8) -> sme::ScanResult {
    let mut result = scan_result(ssid, bssid, channel, rssi);
    result.bss_description.capability_info = 1;
    result.bss_description.ies.truncate(2 + ssid.len() + 6);
    result.compatibility = sme::Compatibility::Compatible(sme::Compatible {
        mutual_security_protocols: vec![internal::Protocol::Open],
    });
    result
}

fn seed(state: &TestDirectory) -> anyhow::Result<()> {
    let (tx, _rx) = mpsc::channel::<TelemetryEvent>(8);
    let manager = futures::executor::block_on(SavedNetworksManager::new_with_directory(
        state.capability()?,
        TelemetrySender::new(tx),
    ))?;
    let _ = futures::executor::block_on(manager.store(
        id(SSID, SecurityType::Wpa3),
        Credential::Password(PASSWORD.to_vec()),
    ))
    .map_err(|_| anyhow::anyhow!("failed to seed selected network"))?;
    let _ = futures::executor::block_on(
        manager.store(id(b"decoy-network", SecurityType::None), Credential::None),
    )
    .map_err(|_| anyhow::anyhow!("failed to seed decoy network"))?;
    Ok(())
}

fn load_selected(
    state: &TestDirectory,
) -> anyhow::Result<wlancfg_selection::config_management::NetworkConfig> {
    let (tx, _rx) = mpsc::channel::<TelemetryEvent>(8);
    let manager = futures::executor::block_on(SavedNetworksManager::new_with_directory(
        state.capability()?,
        TelemetrySender::new(tx),
    ))?;
    futures::executor::block_on(manager.lookup(&id(SSID, SecurityType::Wpa3)))
        .ok_or_else(|| anyhow::anyhow!("selected network disappeared from persistence"))
}

fn id(ssid: &[u8], security: SecurityType) -> NetworkIdentifier {
    NetworkIdentifier::new(types::Ssid::from_bytes_unchecked(ssid.to_vec()), security)
}

fn field(line: &str, name: &str) -> u64 {
    line.split_whitespace()
        .find_map(|part| {
            part.strip_prefix(&format!("{name}="))
                .and_then(|value| value.parse().ok())
        })
        .unwrap()
}

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!("drv-wlancfg-e2e-{}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
    fn capability(&self) -> io::Result<File> {
        OpenOptions::new().read(true).open(&self.0)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
