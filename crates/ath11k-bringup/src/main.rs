#![forbid(unsafe_code)]

use ath11k_bringup::{Cli, DryRunHost, RealHost, preflight, run};
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

struct RunnerHeartbeat {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl RunnerHeartbeat {
    fn start() -> Result<Self, String> {
        let directory = std::path::Path::new("/run/redwood-lab-watchdog");
        fs::create_dir_all(directory)
            .map_err(|error| format!("prepare watchdog runtime directory: {error}"))?;
        fs::write(
            directory.join("runner.pid"),
            format!("{}\n", std::process::id()),
        )
        .map_err(|error| format!("register runner with watchdog: {error}"))?;
        fs::write(directory.join("heartbeat"), b"alive\n")
            .map_err(|error| format!("start watchdog heartbeat: {error}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let _ = fs::write("/run/redwood-lab-watchdog/heartbeat", b"alive\n");
                thread::sleep(Duration::from_secs(2));
            }
        });
        let heartbeat = Self {
            stop,
            thread: Some(worker),
        };
        for _ in 0..150 {
            if directory.join("armed").is_file() && directory.join("run").is_file() {
                return Ok(heartbeat);
            }
            thread::sleep(Duration::from_millis(200));
        }
        Err("watchdog was not armed and released by the operator within 30 seconds".into())
    }
}

impl Drop for RunnerHeartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}

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
        let heartbeat = match RunnerHeartbeat::start() {
            Ok(heartbeat) => heartbeat,
            Err(message) => {
                eprintln!("ath11k-bringup: {message}");
                std::process::exit(1);
            }
        };
        let mut host = RealHost::default();
        let result = run(&config, &mut host);
        let output = (
            result,
            host.scan_summary().cloned(),
            host.dp_poll_log().to_vec(),
        );
        drop(host);
        drop(heartbeat);
        output
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
