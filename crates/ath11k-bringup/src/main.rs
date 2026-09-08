#![forbid(unsafe_code)]

use ath11k_bringup::{Cli, DryRunHost, RealHost, run};

fn main() {
    let config = match Cli::parse(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let result = if config.dry_run {
        run(&config, &mut DryRunHost::default())
    } else {
        run(&config, &mut RealHost::default())
    };
    match result {
        Ok(stages) => {
            for stage in stages {
                println!("completed {}", stage.as_str());
            }
        }
        Err(error) => {
            eprintln!("ath11k-bringup: {error}");
            std::process::exit(1);
        }
    }
}
