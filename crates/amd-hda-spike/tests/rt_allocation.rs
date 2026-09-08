use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::atomic::{AtomicU64, Ordering},
};

use amd_hda_spike::rt::{
    BDL_ENTRIES, ContinuousDmaRing, DmaProgress, FixedRtExecutor, PcmQuantum, QUANTUM_BYTES,
};

struct AuditedAllocator;
static RT_ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
thread_local! { static IN_RT: Cell<bool> = const { Cell::new(false) }; }

unsafe impl GlobalAlloc for AuditedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        IN_RT.with(|flag| {
            if flag.get() {
                RT_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        IN_RT.with(|flag| {
            if flag.get() {
                RT_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
        });
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: AuditedAllocator = AuditedAllocator;

#[test]
fn fixed_executor_and_ffi_are_rust_allocation_free() {
    let input = PcmQuantum::from_fn(|index| if index % 2 == 0 { 256 } else { -256 });
    let mut executor = FixedRtExecutor::default();
    let mut ring = ContinuousDmaRing::default();
    let mut progress = DmaProgress::default();
    // Initialize this test thread's audit flag before entering the measured region.
    IN_RT.with(|flag| flag.set(false));
    RT_ALLOCATIONS.store(0, Ordering::Relaxed);
    IN_RT.with(|flag| flag.set(true));
    for quantum in 0..10_000 {
        if quantum >= BDL_ENTRIES {
            let completed = quantum % BDL_ENTRIES;
            let completion = quantum - BDL_ENTRIES + 1;
            let lpib = ((completion * QUANTUM_BYTES) % (BDL_ENTRIES * QUANTUM_BYTES)) as u32;
            ring.complete(completed, &mut progress, 1, lpib, false)
                .unwrap();
        }
        executor.execute(&input, &mut ring).unwrap();
    }
    IN_RT.with(|flag| flag.set(false));
    assert_eq!(RT_ALLOCATIONS.load(Ordering::Relaxed), 0);
}

#[test]
fn rt_module_has_no_lock_ipc_logging_or_syscall_surface() {
    let source = include_str!("../src/rt.rs");
    for forbidden in [
        "Mutex",
        "RwLock",
        "mpsc",
        "UnixStream",
        "File::",
        "println!",
        "eprintln!",
        "thread::",
        "Vec<",
        "Box<",
        "fs::",
        "libc::",
        "syscall",
    ] {
        assert!(
            !source.contains(forbidden),
            "forbidden RT token: {forbidden}"
        );
    }
}
