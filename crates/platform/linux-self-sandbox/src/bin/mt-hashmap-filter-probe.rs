use linux_self_sandbox::{Profile, install_runtime_filter_for_integration_test};
use std::collections::{HashMap, HashSet, hash_map::RandomState};
use std::os::fd::RawFd;

fn exercise_runtime_collections(hash_state: RandomState) {
    let mut regions = HashMap::with_hasher(hash_state.clone());
    let mut dmas = HashMap::with_hasher(hash_state.clone());
    let mut quarantined_dmas = HashMap::with_hasher(hash_state.clone());
    let mut interrupts = HashMap::with_hasher(hash_state.clone());
    let mut ambiguous_irqs = HashSet::with_hasher(hash_state.clone());
    let mut failed_releases = HashMap::with_hasher(hash_state.clone());
    for map in [
        &mut regions,
        &mut dmas,
        &mut quarantined_dmas,
        &mut interrupts,
        &mut failed_releases,
    ] {
        assert_eq!(map.insert(1_u8, 2_u8), None);
        assert_eq!(map.get(&1), Some(&2));
    }
    assert!(ambiguous_irqs.insert(1_u8));
    assert!(ambiguous_irqs.contains(&1));

    for map in [
        &mut regions,
        &mut dmas,
        &mut quarantined_dmas,
        &mut interrupts,
        &mut failed_releases,
    ] {
        let old = std::mem::replace(map, HashMap::with_hasher(hash_state.clone()));
        assert_eq!(old.get(&1), Some(&2));
        assert_eq!(map.insert(3_u8, 4_u8), None);
    }
    let old = std::mem::replace(&mut ambiguous_irqs, HashSet::with_hasher(hash_state));
    assert!(old.contains(&1));
    assert!(ambiguous_irqs.insert(3));
}

fn main() {
    let prewarm = match std::env::args().nth(1).as_deref() {
        Some("cold") => false,
        Some("prewarmed") => true,
        _ => std::process::exit(64),
    };
    let prepared_hash_state = prewarm.then(RandomState::new);

    let mut pair = [0; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, pair.as_mut_ptr()) },
        0
    );
    let third = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
    let irq = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(third >= 0 && irq >= 0);
    let fds: [RawFd; 4] = [pair[0], pair[1], third, irq];
    install_runtime_filter_for_integration_test(Profile::Mt7921Vfio {
        pci_config_fd: fds[0],
        vfio_fd: fds[1],
        iommufd: fds[2],
        irq_eventfd: fds[3],
        service: None,
    })
    .unwrap();
    exercise_runtime_collections(prepared_hash_state.unwrap_or_else(RandomState::new));
    unsafe { libc::_exit(0) }
}
