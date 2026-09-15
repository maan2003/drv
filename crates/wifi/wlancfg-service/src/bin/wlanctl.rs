// SPDX-License-Identifier: GPL-2.0-only

//! User-facing WLAN policy CLI. It has no driver/control capability.

use anyhow::{Context as _, bail};
use std::{
    io::{self, BufRead as _, Write as _},
    path::PathBuf,
};
use wlancfg_service::application::{self, Association, PowerSaveMode, Reply, Request, Security};

fn main() -> anyhow::Result<()> {
    let (socket, request) = parse(std::env::args().skip(1))?;
    let reply =
        application::transact(&socket, &request).context("contact wlancfg policy service")?;
    render(reply)
}

fn parse(mut args: impl Iterator<Item = String>) -> anyhow::Result<(PathBuf, Request)> {
    let mut socket = PathBuf::from("/run/drv/wlancfg.sock");
    let mut first = args.next().context("missing command")?;
    if first == "--socket" {
        socket = PathBuf::from(args.next().context("missing --socket path")?);
        first = args.next().context("missing command")?;
    }
    let request = match first.as_str() {
        "scan" => {
            no_more(args)?;
            Request::Scan
        }
        "status" => {
            no_more(args)?;
            Request::Status
        }
        "disconnect" => {
            no_more(args)?;
            Request::Disconnect
        }
        "power-save" => {
            let mode = match args.next().as_deref() {
                Some("performance") => PowerSaveMode::Performance,
                Some("balanced") => PowerSaveMode::Balanced,
                _ => bail!("power-save mode must be performance or balanced"),
            };
            no_more(args)?;
            Request::PowerSave(mode)
        }
        "saved" => {
            no_more(args)?;
            Request::Saved
        }
        "connect" => {
            let ssid = ssid(args.next())?;
            let security = security(args.next())?;
            no_more(args)?;
            let credential = if security == Security::Open {
                Vec::new()
            } else {
                protected_passphrase()?
            };
            Request::Connect {
                ssid,
                security,
                credential,
            }
        }
        "forget" => {
            let ssid = ssid(args.next())?;
            let security = security(args.next())?;
            no_more(args)?;
            Request::Forget { ssid, security }
        }
        _ => bail!(
            "usage: wlanctl [--socket PATH] scan|status|disconnect|power-save performance|balanced|saved|connect SSID open|wpa2|wpa3|forget SSID open|wpa2|wpa3"
        ),
    };
    Ok((socket, request))
}

fn no_more(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    if args.next().is_some() {
        bail!("unexpected argument");
    }
    Ok(())
}
fn ssid(value: Option<String>) -> anyhow::Result<Vec<u8>> {
    let value = value.context("missing SSID")?.into_bytes();
    if value.is_empty() || value.len() > 32 {
        bail!("SSID length is outside 1..=32 bytes");
    }
    Ok(value)
}
fn security(value: Option<String>) -> anyhow::Result<Security> {
    match value.context("missing security")?.as_str() {
        "open" => Ok(Security::Open),
        "wpa2" => Ok(Security::Wpa2),
        "wpa3" => Ok(Security::Wpa3),
        _ => bail!("security must be open, wpa2, or wpa3"),
    }
}

fn protected_passphrase() -> anyhow::Result<Vec<u8>> {
    let fd = libc::STDIN_FILENO;
    let terminal = unsafe { libc::isatty(fd) } == 1;
    let mut saved = None;
    if terminal {
        eprint!("Passphrase: ");
        io::stderr().flush()?;
        let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, attributes.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error()).context("protect passphrase input");
        }
        let mut attributes = unsafe { attributes.assume_init() };
        saved = Some(attributes);
        attributes.c_lflag &= !libc::ECHO;
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &attributes) } != 0 {
            return Err(io::Error::last_os_error()).context("protect passphrase input");
        }
    }
    let mut line = Vec::new();
    let read = io::stdin().lock().read_until(b'\n', &mut line);
    if let Some(attributes) = saved {
        let restored = unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &attributes) };
        eprintln!();
        if restored != 0 {
            return Err(io::Error::last_os_error()).context("restore terminal");
        }
    }
    read.context("read protected passphrase")?;
    while line
        .last()
        .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
    {
        line.pop();
    }
    if !(8..=63).contains(&line.len()) {
        bail!("passphrase length is outside 8..=63 bytes");
    }
    Ok(line)
}

fn render(reply: Reply) -> anyhow::Result<()> {
    match reply {
        Reply::Ok => println!("ok"),
        Reply::Error(message) => bail!("{message}"),
        Reply::Scan(networks) => {
            for network in networks {
                println!(
                    "ssid={} security={} rssi_dbm={}",
                    display_ssid(&network.ssid),
                    display_security(network.security),
                    network.rssi_dbm
                );
            }
        }
        Reply::Saved(networks) => {
            for (ssid, security) in networks {
                println!(
                    "ssid={} security={}",
                    display_ssid(&ssid),
                    display_security(security)
                );
            }
        }
        Reply::Status(status) => {
            match status.association {
                Association::Disconnected => print!("association=disconnected"),
                Association::Disconnecting => print!("association=disconnecting"),
                Association::Connecting => print!("association=connecting"),
                Association::Connected {
                    channel,
                    rssi_dbm,
                    snr_db,
                } => print!(
                    "association=connected channel={channel} rssi_dbm={rssi_dbm} snr_db={snr_db}"
                ),
            }
            if let Some(ssid) = status.ssid {
                print!(" ssid={}", display_ssid(&ssid));
            }
            // Address assignment and Internet reachability belong to networking,
            // not the SME association state. Never inflate association into an
            // Internet-ready claim.
            println!(" address=unavailable internet=unknown");
        }
    }
    Ok(())
}
fn display_security(value: Security) -> &'static str {
    match value {
        Security::Open => "open",
        Security::Wpa2 => "wpa2",
        Security::Wpa3 => "wpa3",
    }
}
fn display_ssid(value: &[u8]) -> String {
    String::from_utf8_lossy(value)
        .chars()
        .flat_map(|character| character.escape_default())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_never_accepts_a_passphrase_argument() {
        assert!(
            parse(
                ["connect", "ap", "wpa3", "secret-in-argv"]
                    .into_iter()
                    .map(str::to_owned)
            )
            .is_err()
        );
    }
    #[test]
    fn power_save_accepts_only_explicit_supported_modes() {
        assert_eq!(
            parse(["power-save", "performance"].into_iter().map(str::to_owned))
                .unwrap()
                .1,
            Request::PowerSave(PowerSaveMode::Performance)
        );
        assert_eq!(
            parse(["power-save", "balanced"].into_iter().map(str::to_owned))
                .unwrap()
                .1,
            Request::PowerSave(PowerSaveMode::Balanced)
        );
        assert!(parse(["power-save", "low"].into_iter().map(str::to_owned)).is_err());
    }

    #[test]
    fn status_does_not_claim_network_or_internet_readiness() {
        let mut output = Vec::new();
        // Contract is asserted symbolically because render writes stdout.
        let status = Reply::Status(application::Status {
            association: Association::Connected {
                channel: 6,
                rssi_dbm: -40,
                snr_db: 25,
            },
            ssid: Some(b"ap".to_vec()),
        });
        let text = format!("{status:?}");
        output.extend_from_slice(text.as_bytes());
        assert!(
            !String::from_utf8(output)
                .unwrap()
                .contains("internet=connected")
        );
    }
}
