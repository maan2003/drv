// SPDX-License-Identifier: GPL-2.0-only

//! Simulated deployment-path acceptance: actual CLI, long-lived wlancfg policy,
//! and Wi-Fi control service run as three separate processes.

use fidl_fuchsia_wlan_ieee80211 as ieee;
use fidl_fuchsia_wlan_internal as internal;
use fidl_fuchsia_wlan_sme as sme;
use futures::channel::mpsc;
use std::{
    fs::{File, OpenOptions},
    io::{self, Write as _},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStrExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
use wifi_control_service::{PreparedServer, RuntimeError, WifiRuntime};
const GENERATION: [u8; 16] = [0x71; 16];
const SSID: &[u8] = b"selected-network";
const SECOND_SSID: &[u8] = b"second-target";
const PASSWORD: &[u8] = b"selected-password";
const BSSID: [u8; 6] = [2, 7, 1, 2, 3, 4];
const SECOND_BSSID: [u8; 6] = [2, 7, 1, 2, 3, 5];

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
    let socket_path = state.path().join("wlancfg.sock");
    let listener = application_listener(&socket_path)?;

    let generation = start_generation(&state, &listener, "retry-success")?;
    let scan = cli(&socket_path, &["scan"], None)?;
    require_success(&scan, "scan")?;
    assert!(String::from_utf8_lossy(&scan.stdout).contains("ssid=selected-network security=wpa3"));

    let connect = cli(
        &socket_path,
        &["connect", "selected-network", "wpa3"],
        Some(PASSWORD),
    )?;
    require_success(&connect, "connect")?;
    let status = cli(&socket_path, &["status"], None)?;
    require_success(&status, "status")?;
    let status = String::from_utf8(status.stdout)?;
    assert!(status.contains("association=connected"), "{status}");
    assert!(
        status.contains("address=unavailable internet=unknown"),
        "{status}"
    );

    let saved = cli(&socket_path, &["saved"], None)?;
    require_success(&saved, "saved")?;
    assert!(String::from_utf8_lossy(&saved.stdout).contains("ssid=selected-network security=wpa3"));

    let disconnect = cli(&socket_path, &["disconnect"], None)?;
    require_success(&disconnect, "disconnect")?;
    let status = cli(&socket_path, &["status"], None)?;
    assert!(String::from_utf8_lossy(&status.stdout).contains("association=disconnected"));
    std::thread::sleep(Duration::from_millis(150));
    let reconnect = cli(
        &socket_path,
        &["connect", "selected-network", "wpa3"],
        Some(PASSWORD),
    )?;
    require_success(&reconnect, "explicit reconnect")?;
    wait_status(&socket_path, "association=connected")?;
    let first_wifi = generation.stop()?;
    let first_connects = lines(&first_wifi, "CONNECT ");
    assert_eq!(
        first_connects.len(),
        5,
        "retry + explicit reconnect count: {first_wifi}"
    );
    let times: Vec<u64> = first_connects[..4]
        .iter()
        .map(|line| field(line, "ms"))
        .collect();
    for (actual, expected) in times
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .zip([400u64, 800, 1200])
    {
        assert!(actual.abs_diff(expected) < 180, "retry times: {times:?}");
    }
    assert!(
        !first_wifi
            .as_bytes()
            .windows(PASSWORD.len())
            .any(|bytes| bytes == PASSWORD),
        "Wi-Fi logs exposed credential"
    );

    // The same persisted store is loaded by a fresh daemon process, which
    // automatically selects and reconnects through the same policy/control path.
    let restarted = start_generation(&state, &listener, "success")?;
    wait_status(&socket_path, "association=connected")?;
    let reconnect_status = cli(&socket_path, &["status"], None)?;
    require_success(&reconnect_status, "restarted status")?;

    require_success(
        &cli(&socket_path, &["forget", "selected-network", "wpa3"], None)?,
        "forget",
    )?;
    let saved = cli(&socket_path, &["saved"], None)?;
    require_success(&saved, "saved after forget")?;
    assert!(saved.stdout.is_empty(), "forgotten network remained saved");
    let second_wifi = restarted.stop()?;
    assert_eq!(
        lines(&second_wifi, "CONNECT ").len(),
        1,
        "unexpected connect count: {second_wifi}"
    );

    // With no saved candidate the production policy loop remains available and
    // correctly reports offline rather than exiting or inventing a lab path.
    let forgotten = start_generation(&state, &listener, "credential-rejected")?;
    wait_status(&socket_path, "association=disconnected")?;
    let scan_again = cli(&socket_path, &["scan"], None)?;
    require_success(&scan_again, "scan after forget")?;
    let rejected = cli(
        &socket_path,
        &["connect", "selected-network", "wpa3"],
        Some(PASSWORD),
    )?;
    assert!(
        !rejected.status.success(),
        "credential rejection was reported as success"
    );
    wait_status(&socket_path, "association=disconnected")?;
    require_success(
        &cli(&socket_path, &["forget", "selected-network", "wpa3"], None)?,
        "forget rejected credential",
    )?;
    let third_wifi = forgotten.stop()?;
    assert_eq!(
        lines(&third_wifi, "CONNECT ").len(),
        1,
        "forgotten network autoconnected or rejection retried: {third_wifi}"
    );
    drop(listener);
    drop(state);

    // A new explicit request must cross an acknowledged disconnect boundary.
    // The old Connected status cannot satisfy or relabel the second request.
    let switch_state = TestDirectory::new()?;
    let switch_socket = switch_state.path().join("wlancfg.sock");
    let switch_listener = application_listener(&switch_socket)?;
    let switching = start_generation(&switch_state, &switch_listener, "second-target-rejected")?;
    require_success(
        &cli(
            &switch_socket,
            &["connect", "selected-network", "wpa3"],
            Some(PASSWORD),
        )?,
        "first target",
    )?;
    let second = cli(
        &switch_socket,
        &["connect", "second-target", "wpa3"],
        Some(PASSWORD),
    )?;
    assert!(
        !second.status.success(),
        "failed second target accepted the old Connected status"
    );
    let status = cli(&switch_socket, &["status"], None)?;
    let status = String::from_utf8(status.stdout)?;
    assert!(
        status.contains("association=disconnected") && status.contains("ssid=second-target"),
        "fresh machine did not retain its target identity after failure: {status}"
    );
    let switching_wifi = switching.stop()?;
    assert!(
        lines(&switching_wifi, "CONNECT ").len() >= 2,
        "second request never reached Wi-Fi runtime: {switching_wifi}"
    );

    Ok(())
}

struct Generation {
    policy: Child,
    wifi: Child,
}
impl Generation {
    fn stop(mut self) -> anyhow::Result<String> {
        let _ = self.policy.kill();
        let _ = self.wifi.kill();
        let policy = self.policy.wait()?;
        let wifi = self.wifi.wait_with_output()?;
        assert!(
            !policy.success(),
            "fixture policy unexpectedly exited itself"
        );
        Ok(String::from_utf8(wifi.stdout)?)
    }
}

fn start_generation(
    state: &TestDirectory,
    listener: &OwnedFd,
    scenario: &str,
) -> anyhow::Result<Generation> {
    let (policy_parent, policy_wifi) = sockets()?;
    let (supervisor_parent, supervisor_wifi) = sockets()?;
    let state_fd = state.capability()?;
    let wifi = spawn_role(
        "wifi",
        scenario,
        policy_wifi.as_raw_fd(),
        supervisor_wifi.as_raw_fd(),
        None,
    )?;
    let policy = spawn_role(
        "wlancfg-fixture-no-kernel-sandbox",
        scenario,
        policy_parent.as_raw_fd(),
        state_fd.as_raw_fd(),
        Some(listener.as_raw_fd()),
    )?;
    inspect_stopped_child(&wifi, &[3, 4])?;
    inspect_stopped_child(&policy, &[3, 4, 5])?;
    drop(policy_parent);
    drop(policy_wifi);
    drop(supervisor_wifi);
    drop(supervisor_parent);
    drop(state_fd);
    Ok(Generation { policy, wifi })
}

fn cli(path: &Path, args: &[&str], input: Option<&[u8]>) -> anyhow::Result<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wlanctl"));
    command
        .arg("--socket")
        .arg(path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = command.spawn()?;
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input)?;
    }
    Ok(child.wait_with_output()?)
}
fn require_success(output: &Output, operation: &str) -> anyhow::Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        anyhow::bail!("{operation}: {}", String::from_utf8_lossy(&output.stderr))
    }
}
fn wait_status(path: &Path, expected: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let output = cli(path, &["status"], None)?;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).contains(expected) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {expected}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn lines<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    text.lines()
        .filter(|line| line.starts_with(prefix))
        .collect()
}
fn field(line: &str, name: &str) -> u64 {
    line.split_whitespace()
        .find_map(|part| {
            part.strip_prefix(&format!("{name}="))
                .and_then(|value| value.parse().ok())
        })
        .unwrap()
}

fn spawn_role(
    role: &str,
    scenario: &str,
    fd3: RawFd,
    fd4: RawFd,
    fd5: Option<RawFd>,
) -> io::Result<Child> {
    let fd3 = duplicate_high(fd3)?;
    let fd4 = duplicate_high(fd4)?;
    let fd5 = fd5.map(duplicate_high).transpose()?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .env("DRV_WLANCFG_E2E_ROLE", role)
        .env("DRV_WLANCFG_E2E_SCENARIO", scenario)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            for (source, target) in [(fd3.as_raw_fd(), 3), (fd4.as_raw_fd(), 4)] {
                if libc::dup2(source, target) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if let Some(source) = &fd5
                && libc::dup2(source.as_raw_fd(), 5) < 0
            {
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
fn inspect_stopped_child(child: &Child, inherited: &[RawFd]) -> anyhow::Result<()> {
    let pid = child.id();
    let mut status = 0;
    if unsafe { libc::waitpid(pid as _, &mut status, libc::WUNTRACED) } != pid as _
        || !libc::WIFSTOPPED(status)
    {
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
    let mut expected = vec![0, 1, 2];
    expected.extend(inherited);
    expected.sort_unstable();
    assert_eq!(fds, expected);
    if unsafe { libc::kill(pid as _, libc::SIGCONT) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
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
fn application_listener(path: &Path) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as _;
    for (to, from) in address.sun_path.iter_mut().zip(bytes.iter().copied()) {
        *to = from as _;
    }
    let length = std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1;
    if unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length as _,
        )
    } != 0
        || unsafe { libc::listen(fd.as_raw_fd(), 8) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

fn wlancfg_child() -> anyhow::Result<()> {
    capability_checkpoint()?;
    let control = unsafe { OwnedFd::from_raw_fd(3) };
    let state = unsafe { OwnedFd::from_raw_fd(4) };
    let applications = unsafe { OwnedFd::from_raw_fd(5) };
    let prepared =
        wlancfg_service::PreparedHostControlClient::from_inherited_socket(control, GENERATION)?;
    let prepared_applications =
        wlancfg_service::application::PreparedApplicationServer::from_inherited_listener(
            applications,
        )?;
    let (tx, rx) = mpsc::channel(15);
    let parked = prepared.spawn_parked_after_setup()?;
    let parked_applications = prepared_applications.spawn_parked(tx)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    wlancfg_service::policy::serve(runtime, parked, parked_applications, rx, state)
}
fn wifi_child() -> anyhow::Result<()> {
    capability_checkpoint()?;
    let policy = unsafe { OwnedFd::from_raw_fd(3) };
    let supervisor = unsafe { OwnedFd::from_raw_fd(4) };
    let runtime = FixtureWifi::new(std::env::var("DRV_WLANCFG_E2E_SCENARIO")?);
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
        let second_target_rejected = self.scenario == "second-target-rejected"
            && request.bss_description.bssid == SECOND_BSSID;
        let fail = self.scenario == "credential-rejected"
            || second_target_rejected
            || (self.scenario == "retry-success" && self.attempts < 4);
        if fail {
            return Err(RuntimeError::Failed(sme::ConnectResult {
                code: ieee::StatusCode::RefusedReasonUnspecified,
                is_credential_rejected: self.scenario == "credential-rejected"
                    || second_target_rejected,
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
        let mut results = vec![
            scan_result(SSID, BSSID, 6, if active { -35 } else { -42 }),
            scan_result(
                SECOND_SSID,
                SECOND_BSSID,
                11,
                if active { -34 } else { -41 },
            ),
        ];
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
