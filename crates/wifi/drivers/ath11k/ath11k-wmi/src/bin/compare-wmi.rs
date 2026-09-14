use ath11k_wmi::cmd::comparison::compare_jsonl;

fn main() {
    let mut args = std::env::args_os();
    let program = args.next().unwrap_or_default();
    let (Some(native_path), Some(runner_path), None) = (args.next(), args.next(), args.next())
    else {
        eprintln!(
            "usage: {} <native-ordered.jsonl> <runner.jsonl>",
            std::path::Path::new(&program)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        );
        std::process::exit(2);
    };
    let native = std::fs::read_to_string(&native_path).unwrap_or_else(|error| {
        eprintln!("failed to read {}: {error}", native_path.to_string_lossy());
        std::process::exit(2);
    });
    let runner = std::fs::read_to_string(&runner_path).unwrap_or_else(|error| {
        eprintln!("failed to read {}: {error}", runner_path.to_string_lossy());
        std::process::exit(2);
    });
    let comparison = compare_jsonl(&native, &runner).unwrap_or_else(|error| {
        eprintln!("invalid transcript: {error:?}");
        std::process::exit(2);
    });
    print!("{}", comparison.deterministic_report());
}
