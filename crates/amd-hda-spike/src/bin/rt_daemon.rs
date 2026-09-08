use amd_hda_spike::rt::{
    BDL_ENTRIES, ContinuousDmaRing, DmaProgress, FixedRtExecutor, PcmQuantum, QUANTUM_BYTES,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next();
    if !matches!(
        mode.as_deref(),
        Some("--virtual-audit" | "--virtual-daemon")
    ) {
        eprintln!(
            "Phase 1A hardware output is disabled; use --virtual-audit [quanta] or --virtual-daemon"
        );
        std::process::exit(2);
    }
    let audit_quanta = (mode.as_deref() == Some("--virtual-audit")).then(|| {
        args.next()
            .map(|value| value.parse::<u64>().expect("quanta must be an integer"))
            .unwrap_or(10_000)
    });
    if audit_quanta == Some(0) {
        eprintln!("quanta must be greater than zero");
        std::process::exit(2);
    }
    if audit_quanta.is_none() {
        let worker = std::thread::Builder::new()
            .name("audio-rt-virtual".into())
            .spawn(|| run_virtual(None))
            .expect("start virtual executor thread");
        println!("hardware-disabled virtual daemon process ready");
        worker.join().unwrap();
        return;
    }
    let (quantum, periods, report) = run_virtual(audit_quanta);
    println!(
        "virtual RT audit: quanta={} periods={} frames={} peak={} checksum={}",
        quantum, periods, report.frame_position, report.protected_peak, report.protected_checksum
    );
}

fn run_virtual(audit_quanta: Option<u64>) -> (u64, u64, amd_hda_spike::rt::QuantumReport) {
    let input = PcmQuantum::from_fn(|index| if index % 2 == 0 { 256 } else { -256 });
    let mut executor = FixedRtExecutor::default();
    let mut ring = ContinuousDmaRing::default();
    let mut progress = DmaProgress::default();
    let mut quantum = 0_u64;
    loop {
        if quantum >= BDL_ENTRIES as u64 {
            let completed = quantum as usize % BDL_ENTRIES;
            let completion = quantum - BDL_ENTRIES as u64 + 1;
            let lpib =
                ((completion as usize * QUANTUM_BYTES) % (BDL_ENTRIES * QUANTUM_BYTES)) as u32;
            ring.complete(completed, &mut progress, 1, lpib, false)
                .unwrap();
        }
        let report = executor.execute(&input, &mut ring).unwrap();
        quantum += 1;
        if audit_quanta == Some(quantum) {
            return (quantum, ring.submitted_periods(), report);
        }
        if audit_quanta.is_none() {
            // Hardware-free stand-in for the permitted HDA completion wait;
            // it is outside the audited executor call.
            std::thread::sleep(std::time::Duration::from_micros(10_000));
        }
    }
}
