use mt7921_port_spike::frequency_to_channel;
use std::{env, process::Command};

#[derive(Default)]
struct Bss {
    bssid: String,
    ssid: String,
    frequency: u16,
    signal: String,
    capability: String,
    rsn: bool,
    wpa: bool,
    personal: bool,
    enterprise: bool,
    sae: bool,
    owe: bool,
}

fn main() {
    let interface = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: mt7921-scan INTERFACE");
        std::process::exit(2)
    });
    let output = Command::new("iw")
        .args(["dev", &interface, "scan"])
        .output()
        .unwrap_or_else(|error| fail(&format!("cannot execute iw: {error}")));
    if !output.status.success() {
        fail(&String::from_utf8_lossy(&output.stderr));
    }
    let text = String::from_utf8(output.stdout).unwrap_or_else(|_| fail("iw output is not UTF-8"));
    let mut current = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("BSS ") {
            emit(current.take());
            let bssid = rest.split(['(', ' ']).next().unwrap_or_default().to_owned();
            current = Some(Bss {
                bssid,
                ..Bss::default()
            });
            continue;
        }
        let Some(bss) = current.as_mut() else {
            continue;
        };
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("freq: ") {
            // iw 6.17 prints MHz with a `.0` suffix.
            bss.frequency = value
                .split_once('.')
                .map_or(value, |(integer, _)| integer)
                .parse()
                .unwrap_or(0);
        } else if let Some(value) = trimmed.strip_prefix("signal: ") {
            bss.signal = value.split_whitespace().next().unwrap_or("0").to_owned();
        } else if let Some(value) = trimmed.strip_prefix("SSID: ") {
            bss.ssid = value.to_owned();
        } else if let Some(value) = trimmed.strip_prefix("capability: ") {
            bss.capability = value
                .rsplit_once('(')
                .map_or(value, |x| x.0)
                .trim()
                .to_owned();
        } else if trimmed == "RSN:" {
            bss.rsn = true;
        } else if trimmed == "WPA:" {
            bss.wpa = true;
        } else if let Some(suites) = trimmed.strip_prefix("* Authentication suites:") {
            bss.personal |= suites.contains("PSK");
            bss.enterprise |= suites.contains("IEEE 802.1X");
            bss.sae |= suites.contains("SAE");
            bss.owe |= suites.contains("OWE");
        }
    }
    emit(current);
}

fn emit(bss: Option<Bss>) {
    let Some(bss) = bss else { return };
    let privacy = bss.capability.split_whitespace().any(|x| x == "Privacy");
    let security = if bss.owe {
        "owe"
    } else if bss.sae {
        "wpa3-personal"
    } else if bss.enterprise {
        "wpa2-enterprise"
    } else if bss.personal || bss.rsn {
        "wpa2-personal"
    } else if bss.wpa {
        "wpa1"
    } else if privacy {
        "wep-or-unknown-protected"
    } else {
        "open"
    };
    println!(
        "{{\"bssid\":\"{}\",\"ssid\":\"{}\",\"frequency_mhz\":{},\"channel\":{},\"signal_dbm\":{},\"security\":\"{}\",\"capabilities\":\"{}\"}}",
        json(&bss.bssid),
        json(&bss.ssid),
        bss.frequency,
        frequency_to_channel(bss.frequency),
        bss.signal,
        security,
        json(&bss.capability)
    );
}

fn json(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn fail(message: &str) -> ! {
    eprintln!("mt7921-scan: {message}");
    std::process::exit(1)
}
