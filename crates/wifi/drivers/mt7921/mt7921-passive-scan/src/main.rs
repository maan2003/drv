// SPDX-License-Identifier: GPL-2.0-only

//! Thin MT7921 service composition. Hardware/DMA stay inside Mt7921Driver;
//! MLME/SME and that driver run under one ClientRuntime, without radio IPC.

use mt7921_production_client::{
    FirmwareImageExpectation, Mt7921Driver, Mt7921HardwareSessionConfig, RegulatoryDatabaseFile,
    VerifiedFirmwareImages,
};
use std::{
    env,
    fs::File,
    io::Read,
    os::fd::{FromRawFd, OwnedFd, RawFd},
    process::Command,
};
use wlan_softmac_host::runtime::{ClientRuntime, PreparedRuntimeResources};

const PATCH_BYTES: usize = 92_192;
const RAM_BYTES: usize = 792_036;
const PATCH_SHA256: &str = "a276c06c2b772adb50b86639d33c82824ff4c21d617feb78caea74c040b873f6";
const RAM_SHA256: &str = "b94217a951518a9c14095765f367bc5dd7698f2dc033941d6f18fc2ebd6a2ab9";

fn main() {
    if let Err(error) = run() {
        eprintln!("mt7921-service: {error}");
        std::process::exit(1);
    }
}

fn required(name: &str) -> Result<String, String> {
    env::var(name).map_err(|_| format!("{name} is required"))
}

fn hex<const N: usize>(value: &str) -> Result<[u8; N], String> {
    if value.len() != N * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid hexadecimal identity".into());
    }
    let mut bytes = [0; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "invalid hexadecimal identity")?;
    }
    Ok(bytes)
}

fn firmware(path: &str, length: usize) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| format!("open firmware: {error}"))?;
    let mut bytes = Vec::with_capacity(length);
    file.take(length as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read firmware: {error}"))?;
    Ok(bytes)
}

// Preserve the existing activation gate while physical recovery qualification
// remains outstanding. This check is setup-only and never owns device/DMA.
fn require_armed_watchdog() -> Result<(), String> {
    let output = Command::new("/run/current-system/sw/bin/wifi-lab-watchdog")
        .arg("status")
        .output()
        .map_err(|error| format!("query recovery watchdog: {error}"))?;
    if !output.status.success() {
        return Err("recovery watchdog status failed".into());
    }
    let status = std::str::from_utf8(&output.stdout).map_err(|_| "invalid watchdog status")?;
    let mut lines = status.lines();
    let deadline = lines
        .next()
        .unwrap_or_default()
        .strip_prefix("armed deadline=")
        .ok_or("recovery watchdog is not armed")?
        .parse::<u64>()
        .map_err(|_| "invalid watchdog deadline")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "invalid wall clock")?
        .as_secs();
    let remainder = lines.collect::<Vec<_>>();
    if deadline <= now
        || !remainder.contains(&"ActiveState=active")
        || !remainder.contains(&"SubState=waiting")
    {
        return Err("recovery watchdog is not active and waiting".into());
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--describe"] {
        println!(
            "mt7921-service owner=typed-driver protocol=mlme+sme process=single radio_operations=passive-scan+channel20+peer old_owner=removed"
        );
        return Ok(());
    }
    if args != ["--run-wifi-service"] {
        return Err(
            "usage: mt7921-passive-scan --run-wifi-service | --describe; legacy lab modes removed"
                .into(),
        );
    }
    let generation = hex::<16>(&required("DRV_WIFI_GENERATION")?)?;
    if generation == [0; 16] {
        return Err("zero service generation".into());
    }
    let mac = hex::<6>(&required("DRV_SAE_CLIENT_MAC")?.replace(':', ""))?;
    if mac == [0; 6] || mac[0] & 1 != 0 {
        return Err("invalid unicast MAC".into());
    }
    let policy_fd = required("DRV_WIFI_POLICY_FD")?
        .parse::<RawFd>()
        .map_err(|_| "invalid policy FD")?;
    let supervisor_fd = required("DRV_WIFI_SUPERVISOR_FD")?
        .parse::<RawFd>()
        .map_err(|_| "invalid supervisor FD")?;
    let regulatory_fd = required("DRV_REGULATORY_DATABASE_FD")?
        .parse::<RawFd>()
        .map_err(|_| "invalid regulatory FD")?;
    let regulatory_length = required("DRV_REGULATORY_DATABASE_LEN")?
        .parse::<usize>()
        .map_err(|_| "invalid regulatory length")?;
    let regulatory_sha256 = hex::<32>(&required("DRV_REGULATORY_DATABASE_SHA256")?)?;
    if [policy_fd, supervisor_fd, regulatory_fd]
        .iter()
        .any(|fd| *fd < 3)
        || policy_fd == supervisor_fd
        || regulatory_fd == policy_fd
        || regulatory_fd == supervisor_fd
    {
        return Err("inherited descriptors must be distinct non-stdio capabilities".into());
    }
    for fd in [policy_fd, supervisor_fd, regulatory_fd] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            return Err(format!(
                "validate inherited control FD: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    // Only the entrypoint adopts inherited process descriptors. The driver
    // never receives policy IPC or creates a second hardware owner.
    let database = RegulatoryDatabaseFile::adopt(unsafe { File::from_raw_fd(regulatory_fd) });
    let endpoints = wifi_control_service::PreparedServerEndpoints::new(
        unsafe { OwnedFd::from_raw_fd(policy_fd) },
        unsafe { OwnedFd::from_raw_fd(supervisor_fd) },
        generation,
    )
    .map_err(|error| format!("validate service endpoints: {error}"))?;
    let images = VerifiedFirmwareImages::verify(
        firmware(&required("DRV_MT7921_PATCH_IMAGE")?, PATCH_BYTES)?,
        firmware(&required("DRV_MT7921_RAM_IMAGE")?, RAM_BYTES)?,
        FirmwareImageExpectation {
            length: PATCH_BYTES,
            sha256: hex(PATCH_SHA256)?,
        },
        FirmwareImageExpectation {
            length: RAM_BYTES,
            sha256: hex(RAM_SHA256)?,
        },
    )
    .map_err(|error| format!("verify firmware: {error:?}"))?;
    let before = linux_self_sandbox::open_fd_snapshot().map_err(|error| error.to_string())?;
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| format!("prepare executor: {error}"))?;
    let local = tokio::task::LocalSet::new();
    let runtime_fds = linux_self_sandbox::open_fd_snapshot()
        .map_err(|error| error.to_string())?
        .difference(&before)
        .copied()
        .collect();
    let resources = PreparedRuntimeResources::new(mac)
        .map_err(|error| format!("prepare protocol runtime: {error}"))?;
    let [control_fd, supervisor_fd] = endpoints.fd_identities();
    let service = linux_self_sandbox::WifiServiceFds {
        control_fd,
        supervisor_fd,
        regulatory_fd: Some(regulatory_fd),
        ethernet_fds: resources.fd_identities(),
        runtime_fds,
    };
    require_armed_watchdog()?;
    let setup =
        Mt7921HardwareSessionConfig::setup(required("DRV_VFIO_DEVICE")?, &required("DRV_PCI_BDF")?)
            .map_err(|error| format!("prepare device capabilities: {error:?}"))?;
    let setup = {
        let _entered = executor.enter();
        setup
            .with_async_interrupt()
            .map_err(|error| format!("register device IRQ: {error:?}"))?
    };
    let config = setup
        .lock_down_with_service(service)
        .map_err(|error| format!("lock down MT7921 service: {error}"))?;
    local.block_on(&executor, async move {
        let database = database
            .verify(regulatory_length, regulatory_sha256)
            .map_err(|error| format!("verify regulatory database: {error}"))?;
        let mut driver = Mt7921Driver::initialize(config, images, database)
            .map_err(|error| format!("initialize MT7921: {error:?}"))?;
        if driver.firmware().nic_capability.mac_address != Some(mac) {
            return Err(
                "configured MAC differs from firmware identity; MAC override is unavailable".into(),
            );
        }
        // One capability source for both SME selection and MLME joining.
        let device_info = wlan_mlme::mlme_device_info_from_softmac(
            wlan_softmac_host::WlanSoftmac::query(&mut driver)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let runtime = ClientRuntime::new_with_prepared_resources(
            driver,
            wlan_sme::client::ClientConfig::default(),
            device_info,
            Default::default(),
            Default::default(),
            fuchsia_inspect::Inspector::default(),
            resources,
        )
        .await
        .map_err(|error| format!("construct protocol runtime: {error}"))?;
        let mut server = endpoints
            .bind_runtime(runtime)
            .post_lockdown_open_complete()
            .map_err(|error| format!("open control generation: {error}"))?;
        eprintln!(
            "mt7921_service=READY owner=typed-driver radio_operations=passive-scan+channel20+peer"
        );
        let result = server.run_to_terminal().await;
        let mut runtime = server.into_runtime();
        let stopped = runtime.shutdown().await;
        result.map_err(|error| format!("control service: {error}"))?;
        stopped.map_err(|error| format!("contain driver: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_checked_before_any_device_access() {
        assert_eq!(hex::<2>("aB01").unwrap(), [0xab, 1]);
        for invalid in ["", "abc", "abcde", "zzzz", "éé"] {
            assert!(hex::<2>(invalid).is_err());
        }
    }
}
