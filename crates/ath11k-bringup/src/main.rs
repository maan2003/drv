#![forbid(unsafe_code)]

use ath11k_bringup::{Cli, DryRunHost, RealHost, preflight, run};

fn main() {
    let config = match Cli::parse(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    if config.preflight {
        match preflight(&config) {
            Ok(lines) => {
                for line in lines {
                    println!("{line}");
                }
                return;
            }
            Err(error) => {
                eprintln!("ath11k-bringup: {error}");
                std::process::exit(1);
            }
        }
    }
    let (result, summary, dp_poll_log) = if config.dry_run {
        let mut host = DryRunHost::default();
        let result = run(&config, &mut host);
        (
            result,
            host.scan_summary().cloned(),
            host.dp_poll_log().to_vec(),
        )
    } else {
        let mut host = RealHost::default();
        let result = run(&config, &mut host);
        (
            result,
            host.scan_summary().cloned(),
            host.dp_poll_log().to_vec(),
        )
    };
    match result {
        Ok(stages) => {
            for stage in stages {
                println!("completed {}", stage.as_str());
            }
            if let Some(summary) = summary {
                if let Some(selected) = summary.selected {
                    println!("selected {selected}");
                } else {
                    for bss in summary.bsses {
                        println!("bss {bss}");
                    }
                }
            }
            for line in dp_poll_log {
                println!("{line}");
            }
        }
        Err(error) => {
            eprintln!("ath11k-bringup: {error}");
            std::process::exit(1);
        }
    }
}
