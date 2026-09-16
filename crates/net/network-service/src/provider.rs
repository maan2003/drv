// SPDX-License-Identifier: GPL-2.0-only
//! Linux service setup and scheduling for the per-socket binding.
//! ABI is defined by kernel-provider/production/protocol.h.
use crate::socket_worker::{SocketWorker, Work};
use netstack3_port_integration::Runtime;
use netstack3_port_spike::{
    EthernetDevice as _, EthernetEventSource as _, NetworkServiceEndpoint,
    StackEthernetEndpoint as _,
};
use rand::SeedableRng as _;
use std::collections::HashMap;
use std::io;
use std::num::NonZeroU64;
use std::os::fd::{AsFd as _, AsRawFd, FromRawFd, OwnedFd};
use std::rc::Rc;
use std::time::Instant;
const CLAIM: libc::c_ulong = 0x8008B301;

pub(crate) fn read_control(fd: &OwnedFd, bytes: &mut [u8; 128]) -> rustix::io::Result<usize> {
    // SAFETY: this operation writes at most its fixed 128-byte ABI buffer.
    // fd ownership and the writable array remain live throughout the syscall.
    let result = unsafe { libc::ioctl(fd.as_raw_fd(), 0x8080B303 as libc::c_ulong, bytes.as_mut_ptr()) };
    if result < 0 { Err(rustix::io::Errno::from_raw_os_error(io::Error::last_os_error().raw_os_error().unwrap())) }
    else { Ok(result as usize) }
}

/// Resolver endpoint provenance at the privileged process-entry boundary.
pub enum ResolverEndpoint {
    BindDefault,
    Inherited,
}

pub fn run_provider(
    ethernet_mac: Option<[u8; 6]>,
    bootstrap: bool,
    resolver: Option<ResolverEndpoint>,
    link_control: bool,
    netlink: bool,
) -> Result<(), String> {
    if bootstrap && ethernet_mac.is_none() {
        return Err("bootstrap requires an Ethernet identity".into());
    }
    if link_control && ethernet_mac.is_none() {
        return Err("link control requires an Ethernet identity".into());
    }
    // FD3 owns the socket namespace; optional FD4 owns only Ethernet frames.
    if unsafe { libc::fcntl(3, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let mac = ethernet_mac.unwrap_or([2, 0, 0, 0, 0, 1]);
    let mut runtime = Runtime::new_with_capacities(
        512,
        1024,
        std::iter::repeat_with(rand::random::<u8>),
        NonZeroU64::new(1).unwrap(),
        mac,
        u32::from(crate::SOFTMAC_ETHERNET_MTU),
    )
    .map_err(|e| format!("{e:?}"))?;
    if ethernet_mac.is_some() {
        runtime.enable_dynamic_ipv6();
    }
    runtime.enable_loopback();
    let mut network = netstack3_port_integration::service::DhcpService::new(
        runtime,
        rand::rngs::StdRng::from_os_rng(),
        mac,
    );
    let sockets = network.sockets();
    let mut ethernet = if ethernet_mac.is_some() && !link_control {
        let mut kind = 0i32;
        let mut length = std::mem::size_of_val(&kind) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                4,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&mut kind as *mut i32).cast(),
                &mut length,
            )
        } != 0
            || kind != libc::SOCK_SEQPACKET
        {
            return Err("FD4 must be an Ethernet SOCK_SEQPACKET capability".into());
        }
        Some(unsafe { crate::ServiceEthernetDevice::from_frame_fd(OwnedFd::from_raw_fd(4), mac) })
    } else {
        network.on_device_event(netstack3_port_spike::EthernetDeviceEvent::LinkStateChanged(
            false,
        ));
        None
    };
    // Bind before empty-root/no-open sandboxing, never from the DNS engine or NSS.
    // Do not unlink an existing path: another provider may own it.
    let resolver_listener = match resolver {
        Some(ResolverEndpoint::Inherited) => {
            // Validate the transferred endpoint before assuming FD ownership.
            for (option, expected) in [
                (libc::SO_TYPE, libc::SOCK_STREAM),
                (libc::SO_DOMAIN, libc::AF_UNIX),
                (libc::SO_ACCEPTCONN, 1),
            ] {
                let mut value = 0i32;
                let mut length = std::mem::size_of_val(&value) as libc::socklen_t;
                if unsafe {
                    libc::getsockopt(
                        7, libc::SOL_SOCKET, option,
                        (&mut value as *mut i32).cast(), &mut length,
                    )
                } != 0 || value != expected
                {
                    return Err("FD7 must be a listening Unix resolver stream".into());
                }
            }
            // FD7 is transferred exclusively by the supervisor before exec.
            let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(7) };
            listener.set_nonblocking(true).map_err(|e| e.to_string())?;
            Some(listener)
        }
        Some(ResolverEndpoint::BindDefault) => {
            use std::os::unix::fs::PermissionsExt;
            let listener = std::os::unix::net::UnixListener::bind(drv_dns_wire::PATH)
                .map_err(|e| format!("resolver listener: {e}"))?;
            listener.set_nonblocking(true).map_err(|e| e.to_string())?;
            std::fs::set_permissions(drv_dns_wire::PATH, std::fs::Permissions::from_mode(0o666))
                .map_err(|e| e.to_string())?;
            if listener.as_raw_fd() != 7 {
                // SAFETY: before sandbox setup, reserve the documented listener capability
                // slot. The listener owns its original FD; the duplicate gets one owner below.
                if unsafe { libc::dup3(listener.as_raw_fd(), 7, libc::O_CLOEXEC) } < 0 {
                    return Err(io::Error::last_os_error().to_string());
                }
                Some(unsafe { std::os::unix::net::UnixListener::from_raw_fd(7) })
            } else {
                Some(listener)
            }
        }
        None => None,
    };
    crate::child::provider_setup(
        ethernet.is_some(), bootstrap, resolver_listener.is_some(), link_control, netlink,
    )?;
    // SAFETY: setup retains this inherited descriptor exclusively for this
    // provider. Keep its ownership explicit for every ancillary operation.
    let link_control = link_control.then(|| unsafe {
        OwnedFd::from_raw_fd(crate::link_control::CONTROL_FD)
    });
    // SAFETY: provider_setup created FD6; it lives for this entire service loop.
    let poller = unsafe { std::os::fd::BorrowedFd::borrow_raw(6) };
    let mut route_adapter = if netlink {
        // SAFETY: the launcher reserves FD10; setup retains this sole owner.
        let registration = unsafe { OwnedFd::from_raw_fd(crate::rtnetlink::REGISTRATION_FD) };
        Some(crate::rtnetlink::Adapter::new(registration, poller).map_err(|e| e.to_string())?)
    } else { None };
    let mut route_revision = None;
    let mut resolver_server = resolver_listener
        .map(|listener| crate::resolver::ResolverServer::new(listener, poller))
        .transpose()
        .map_err(|e| e.to_string())?;

    if bootstrap {
        crate::child::provider_bootstrap_ready()?;
        // Readiness certifies the initialized core, loopback, registration
        // namespace and sandbox. Physical address acquisition is deliberately
        // not part of provider availability.
        crate::child::provider_bootstrap_network_ready()?;
    }
    eprintln!(
        "netstack3_provider_sandbox_ready=true uid=65534 gid=65534 empty_root=true own_netns=true no_new_privs=true seccomp_default=kill registration_fd=3 endpoint_scope=socket native_loopback=false"
    );
    let mut event = libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: 0, // Socket IDs start at one; zero names registration.
    };
    if unsafe { libc::epoll_ctl(6, libc::EPOLL_CTL_ADD, 3, &mut event) } < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    const ETHERNET_TOKEN: u64 = u64::MAX;
    const LINK_CONTROL_TOKEN: u64 = u64::MAX - 1;
    if let Some(control) = &link_control {
        let mut event = libc::epoll_event {
            events: (libc::EPOLLIN | libc::EPOLLERR | libc::EPOLLHUP) as u32,
            u64: LINK_CONTROL_TOKEN,
        };
        if unsafe {
            libc::epoll_ctl(
                6,
                libc::EPOLL_CTL_ADD,
                control.as_raw_fd(),
                &mut event,
            )
        } < 0
        {
            return Err(io::Error::last_os_error().to_string());
        }
    }
    let mut frame_events = (libc::EPOLLIN | libc::EPOLLERR | libc::EPOLLHUP) as u32;
    if let Some(frame) = &ethernet {
        let mut event = libc::epoll_event {
            events: frame_events,
            u64: ETHERNET_TOKEN,
        };
        if unsafe { libc::epoll_ctl(6, libc::EPOLL_CTL_ADD, frame.raw_fd(), &mut event) } < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
    }
    let mut pending_frame = None;
    let mut last_network_snapshot = None;
    let mut active_link_generation = None;
    let mut last_link_generation = 0u64;
    let mut watch_interfaces = false;
    let mut reported_revision = None;
    let mut report_blocked = false;
    let mut pending_ack = None;
    let mut control_events = (libc::EPOLLIN | libc::EPOLLERR | libc::EPOLLHUP) as u32;
    let mut workers: HashMap<u64, SocketWorker> = HashMap::new();
    let start = Instant::now();
    let mut events = [libc::epoll_event { events: 0, u64: 0 }; 64];
    // Bootstrap registration discovery without polling every endpoint.
    events[0].u64 = 0;
    let mut event_count = 1;
    loop {
        let mut progress = false;
        let mut ready = Vec::with_capacity(64);
        for event in &events[..event_count] {
            if event.u64 == ETHERNET_TOKEN {
                if let Some(frame) = &mut ethernet {
                    frame.notify_epoll(event.events);
                }
            } else if event.u64 != 0
                && event.u64 != crate::resolver::TOKEN
                && event.u64 != LINK_CONTROL_TOKEN
                && !crate::rtnetlink::Adapter::owns_token(event.u64)
            {
                ready.push(event.u64);
            }
        }
        if let Some(control) = &link_control
            && pending_ack.is_none()
            && events[..event_count]
            .iter()
            .any(|event| event.u64 == LINK_CONTROL_TOKEN)
        {
            loop {
                let Some(request) = crate::link_control::receive_request(
                    control.as_fd(),
                )? else {
                    break;
                };
                if let crate::link_control::Request::Watch { generation } = request {
                    watch_interfaces = true;
                    reported_revision = None;
                    pending_ack = Some((generation, crate::link_control::AckStatus::Applied));
                    progress = true;
                    break;
                }
                let generation = match &request {
                    crate::link_control::Request::Attach { generation, .. }
                    | crate::link_control::Request::Detach { generation }
                    | crate::link_control::Request::Watch { generation } => *generation,
                };
                let valid = match &request {
                    crate::link_control::Request::Watch { .. } => unreachable!(),
                    crate::link_control::Request::Attach { generation, .. } => {
                        *generation > last_link_generation
                    }
                    crate::link_control::Request::Detach { generation } => {
                        active_link_generation == Some(*generation)
                    }
                };
                if !valid {
                    pending_ack = Some((generation, crate::link_control::AckStatus::Rejected));
                    progress = true;
                    break;
                }

                if let Some(old) = ethernet.take() {
                    unsafe {
                        libc::epoll_ctl(6, libc::EPOLL_CTL_DEL, old.raw_fd(), std::ptr::null_mut());
                    }
                    network.on_device_event(
                        netstack3_port_spike::EthernetDeviceEvent::LinkStateChanged(false),
                    );
                    pending_frame = None;
                    drop(old);
                }
                match request {
                    crate::link_control::Request::Watch { .. } => unreachable!(),
                    crate::link_control::Request::Attach { generation, frame } => {
                        // The trusted supervisor validates connected AF_UNIX
                        // SOCK_SEQPACKET direction and nonblocking mode before
                        // this capability enters the private channel.
                        let mut installed = unsafe {
                            crate::ServiceEthernetDevice::from_frame_fd(frame, mac)
                        };
                        let mut event = libc::epoll_event {
                            events: frame_events,
                            u64: ETHERNET_TOKEN,
                        };
                        if unsafe {
                            libc::epoll_ctl(6, libc::EPOLL_CTL_ADD, installed.raw_fd(), &mut event)
                        } < 0
                        {
                            return Err(io::Error::last_os_error().to_string());
                        }
                        installed.notify_epoll(libc::EPOLLIN as u32);
                        ethernet = Some(installed);
                        active_link_generation = Some(generation);
                        last_link_generation = generation;
                    }
                    crate::link_control::Request::Detach { .. } => {
                        active_link_generation = None;
                    }
                }
                pending_ack = Some((generation, crate::link_control::AckStatus::Applied));
                progress = true;
                break;
            }
        }
        if events[..event_count].iter().any(|event| event.u64 == 0) {
            for _ in 0..32 {
                let mut id = 0u64;
                let fd = unsafe { libc::ioctl(3, CLAIM, &mut id) };
                if fd < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::WouldBlock {
                        break;
                    }
                    return Err(format!("claim endpoint: {error}"));
                }
                let fd = Rc::new(unsafe { OwnedFd::from_raw_fd(fd) });
                let mut event = libc::epoll_event {
                    events: libc::EPOLLIN as u32,
                    u64: id,
                };
                if unsafe { libc::epoll_ctl(6, libc::EPOLL_CTL_ADD, fd.as_raw_fd(), &mut event) }
                    < 0
                {
                    return Err(io::Error::last_os_error().to_string());
                }
                workers.insert(id, SocketWorker::new(sockets.clone(), id, fd));
                ready.push(id);
                progress = true;
            }
        }
        for id in ready {
            let Some(worker) = workers.get_mut(&id) else {
                continue;
            };
            match worker.handle_requests() {
                Ok(Work::Idle) => {}
                Ok(Work::Progress) => progress = true,
                Ok(Work::Closed) => { workers.remove(&id); progress = true; }
                Err(fault) => {
                    eprintln!("endpoint {id} retired: {fault}");
                    workers.remove(&id);
                    progress = true;
                }
            }
        }

        let mut close_frame = false;
        if let Some(frame) = &mut ethernet {
            while let Some(event) = frame.take_event() {
                if event == netstack3_port_spike::EthernetDeviceEvent::LinkStateChanged(false) {
                    pending_frame = None;
                    close_frame = true;
                    // Retain the service and localhost sockets after link loss.
                    unsafe {
                        libc::epoll_ctl(
                            6,
                            libc::EPOLL_CTL_DEL,
                            frame.raw_fd(),
                            std::ptr::null_mut(),
                        );
                    }
                }
                network.on_device_event(event);
                progress = true;
            }
            for _ in 0..if close_frame { 0 } else { 64 } {
                let Some(packet) = frame.receive() else { break };
                network
                    .receive_frame(packet)
                    .map_err(|_| "Netstack rejected Ethernet frame")?;
                progress = true;
            }
        }
        if close_frame { ethernet = None; }
        progress |= network.poll_at(start.elapsed(), 64) != 0;
        if let Some(frame) = &mut ethernet {
            for _ in 0..64 {
                let Some(packet) = pending_frame.take().or_else(|| network.take_transmit()) else {
                    break;
                };
                match frame.transmit(packet) {
                    Ok(()) => progress = true,
                    Err(packet) => {
                        pending_frame = Some(packet);
                        break;
                    }
                }
            }
            let wanted = (libc::EPOLLIN | libc::EPOLLERR | libc::EPOLLHUP) as u32
                | if frame.wants_write() {
                    libc::EPOLLOUT as u32
                } else {
                    0
                };
            if wanted != frame_events {
                let mut event = libc::epoll_event {
                    events: wanted,
                    u64: ETHERNET_TOKEN,
                };
                if unsafe { libc::epoll_ctl(6, libc::EPOLL_CTL_MOD, frame.raw_fd(), &mut event) }
                    < 0
                {
                    return Err(io::Error::last_os_error().to_string());
                }
                frame_events = wanted;
            }
            let status = network.status();
            let snapshot = {
                let runtime = network.runtime();
                (
                    status,
                    runtime.ipv4_address(),
                    runtime.ipv6_address(),
                    runtime.dns_servers(),
                )
            };
            if last_network_snapshot != Some(snapshot) {
                eprintln!(
                    "provider_network_status={status:?} ipv4={:?} ipv6={:?} dns={:?}",
                    snapshot.1, snapshot.2, snapshot.3,
                );
                last_network_snapshot = Some(snapshot);
            }
        }
        let listeners: Vec<_> = workers
            .iter()
            .filter(|(_, worker)| worker.wants_accept())
            .map(|(&id, _)| id)
            .collect();
        for id in listeners {
            let worker = workers.get_mut(&id).unwrap();
            let mut info = match worker.accept_info() {
                Ok(Some(info)) => info,
                Ok(None) => continue,
                Err(fault) => {
                    eprintln!("endpoint {id} accept retired: {fault}");
                    workers.remove(&id);
                    progress = true;
                    continue;
                }
            };
            let newfd = unsafe {
                libc::ioctl(
                    worker.fd.as_raw_fd(),
                    0xC038B302u64 as libc::c_ulong,
                    info.as_mut_ptr(),
                )
            };
            if newfd < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    worker.pause_accept();
                    continue;
                }
                if matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE | libc::ENOMEM)) {
                    worker.retry_accept();
                    continue;
                }
                eprintln!("endpoint {id} publication retired: {error}");
                workers.remove(&id);
                progress = true;
                continue;
            }
            let child_id = u64::from_le_bytes(info[48..56].try_into().unwrap());
            let fd = Rc::new(unsafe { OwnedFd::from_raw_fd(newfd) });
            // Transfer ownership before registration: failure drops and closes
            // this child, while the listener still owns every unpublished child.
            let child = worker.take_accepted(child_id, fd.clone());
            let mut event = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: child_id,
            };
            if unsafe { libc::epoll_ctl(6, libc::EPOLL_CTL_ADD, fd.as_raw_fd(), &mut event) } < 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            workers.insert(child_id, child);
            progress = true;
        }
        let mut remove = Vec::new();
        for (&id, worker) in workers.iter_mut() {
            match worker.poll_data() {
                Ok(Work::Idle) => {}
                Ok(Work::Progress) => progress = true,
                Ok(Work::Closed) => { remove.push(id); progress = true; }
                Err(fault) => {
                    eprintln!("endpoint {id} retired: {fault}");
                    remove.push(id);
                    progress = true;
                }
            }
        }
        for id in remove {
            workers.remove(&id);
        }
        if let Some(resolver) = &mut resolver_server {
            progress |= resolver
                .poll(&mut network, poller)
                .map_err(|e| format!("resolver: {e}"))?;
        }
        if let Some(control) = &link_control {
            // ACKs are lossless and take precedence over coalesced snapshots.
            // While an ACK is blocked, don't consume another admin request.
            if let Some((generation, status)) = pending_ack {
                if crate::link_control::try_send_ack(control.as_fd(), generation, status)? {
                    pending_ack = None;
                }
            }
            if watch_interfaces && pending_ack.is_none() {
                let revision = network.runtime().interface_revision();
                let writable = events[..event_count].iter().any(|event|
                    event.u64 == LINK_CONTROL_TOKEN && event.events & libc::EPOLLOUT as u32 != 0);
                if reported_revision != Some(revision) && (!report_blocked || writable) {
                    let bytes = serde_json::to_vec(&network.runtime().interface_snapshot())
                        .map_err(|error| format!("interface observation: {error}"))?;
                    let sent = crate::link_control::send_interface(control.as_fd(), &bytes)?;
                    if sent { reported_revision = Some(revision); }
                    report_blocked = !sent;
                }
            }
            let desired = (libc::EPOLLERR | libc::EPOLLHUP
                | if pending_ack.is_none() { libc::EPOLLIN } else { 0 }
                | if report_blocked || pending_ack.is_some() { libc::EPOLLOUT } else { 0 }) as u32;
            if desired != control_events {
                let mut event = libc::epoll_event { events: desired, u64: LINK_CONTROL_TOKEN };
                if unsafe { libc::epoll_ctl(6, libc::EPOLL_CTL_MOD, control.as_raw_fd(), &mut event) } < 0 {
                    return Err(io::Error::last_os_error().to_string());
                }
                control_events = desired;
            }
        }
        if let Some(adapter) = &mut route_adapter {
            let update = {
                let runtime = network.runtime();
                let revision = (runtime.interface_revision(), ethernet.is_some());
                if route_revision != Some(revision) {
                    route_revision = Some(revision);
                    Some(crate::rtnetlink::View::new(
                        runtime.interface_snapshots(), revision.1, mac))
                } else { None }
            };
            progress |= adapter.advance(poller, &events[..event_count], update)
                .map_err(|e| format!("netlink registration: {e}"))?;
        }
        let now = start.elapsed();
        // Poll even while runnable: level-triggered IPC readiness provides fair
        // bounded batches without rescanning every idle endpoint.
        let mut timeout = if progress || network.runtime().has_pending_work() {
            0
        } else {
            network
                .next_timer_deadline()
                .map(|d| {
                    d.saturating_sub(now)
                        .as_millis()
                        .min((i32::MAX - 1) as u128) as i32
                        + 1
                })
                .unwrap_or(-1)
        };
        // Acceptance owns its retry deadline; resource backpressure cannot
        // be represented without the scheduler knowing when it becomes runnable.
        if let Some(deadline) = workers.values().filter_map(|w| w.accept_deadline()).min() {
            let left = deadline.saturating_duration_since(Instant::now()).as_millis()
                .min(i32::MAX as u128) as i32 + 1;
            timeout = if timeout < 0 { left } else { timeout.min(left) };
        }
        if let Some(deadline) = resolver_server.as_ref().and_then(|r| r.deadline()) {
            let left = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(i32::MAX as u128) as i32
                + 1;
            timeout = if timeout < 0 { left } else { timeout.min(left) };
        }
        let n = unsafe {
            libc::syscall(
                libc::SYS_epoll_pwait,
                6u32,
                events.as_mut_ptr(),
                64u32,
                timeout,
                0usize,
                0usize,
            )
        };
        if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error().to_string());
        }
        event_count = n.max(0) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use netstack3_port_integration::NativeTcpBuffers;
    use netstack3_port_integration::socket_provider::NativeSocketProvider;
    use netstack3_port_spike::{RemoteIpVersion, SocketClientId};
    use netstack3_port_spike::provider_transport_v2::{
        ProviderReadinessV2 as Ready, ProviderSocketKindV2,
    };
    use netstack3_tcp::{Buffer, BufferSizes, ReceiveBuffer, SendBuffer};
    use std::cell::RefCell;

    #[test]
    fn loopback_pump_preserves_reentrant_wakes_with_full_event_queue() {
        use std::num::{NonZeroU16, NonZeroUsize};
        use std::time::Duration;
        let mut runtime = Runtime::new_with_capacities(
            8,
            1,
            [7; 8192],
            NonZeroU64::new(1).unwrap(),
            [2, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        runtime.enable_loopback();
        for _ in 0..16 {
            runtime.poll_at(Duration::ZERO, 1);
        }
        let listener = runtime.tcp_socket().unwrap();
        let port = NonZeroU16::new(23462).unwrap();
        runtime
            .tcp_bind(listener, Some([127, 0, 0, 1]), port)
            .unwrap();
        runtime
            .tcp_listen(listener, NonZeroUsize::new(4).unwrap())
            .unwrap();
        let client = runtime.tcp_socket().unwrap();
        runtime.tcp_connect(client, [127, 0, 0, 1], port).unwrap();
        assert!(runtime.has_pending_work());
        assert_eq!(runtime.poll_at(Duration::ZERO, 0), 0);
        assert!(runtime.has_pending_work());
        assert_eq!(runtime.poll_at(Duration::ZERO, 1), 1);
        // Processing SYN enqueues SYN-ACK after the dequeue snapshot was empty.
        assert!(runtime.has_pending_work());
        let accepted = (0..16).find_map(|_| {
            runtime.poll_at(Duration::ZERO, 1);
            runtime.tcp_accept(listener).ok()
        });
        assert!(
            accepted.is_some(),
            "handshake must not wait for a TCP timer"
        );
    }

    #[test]
    fn tcp_ring_wrap_and_payload_slices_preserve_bytes() {
        use netstack3_base::{Payload, PayloadLen};
        let app = NativeTcpBuffers::new(BufferSizes {
            send: 16,
            receive: 16,
        });
        let mut send = app.send.clone();
        let mut receive = app.receive.clone();
        // Fill/consume at offsets that force both ring slices to be used.
        for round in 0..64u8 {
            let input: Vec<_> = (0..16).map(|n| n ^ round).collect();
            assert_eq!(app.write(&input[..11]), 11);
            send.peek_with(0, |p| {
                assert_eq!(receive.write_at(0, &p), 11);
            });
            send.mark_read(11);
            receive.make_readable(11, false);
            let mut out = [0; 11];
            assert_eq!(app.read(&mut out), 11);
            assert_eq!(out, input[..11]);

            assert_eq!(app.write(&input), 16);
            send.peek_with(3, |p| {
                let p = p.slice(2..10);
                assert_eq!(p.len(), 8);
                let mut out = [0; 8];
                p.partial_copy(0, &mut out);
                assert_eq!(out, input[5..13]);
            });
            send.mark_read(16);
        }
    }

    #[test]
    fn tcp_buffer_shrink_waits_for_readable_and_out_of_order_bytes() {
        let app = NativeTcpBuffers::new(BufferSizes {
            send: 16,
            receive: 16,
        });
        let mut send = app.send.clone();
        assert_eq!(app.write(b"abcdefgh"), 8);
        send.request_capacity(4);
        assert_eq!(send.target_capacity(), 4);
        assert_eq!(send.limits().capacity, 16);
        send.mark_read(4);
        assert_eq!(send.limits().capacity, 16);
        send.mark_read(4);
        assert_eq!(send.limits().capacity, 4);
        assert_eq!(app.write(b"12345678"), 4);

        let mut receive = app.receive.clone();
        assert_eq!(receive.write_at(8, &&b"ijkl"[..]), 4);
        receive.request_capacity(4);
        assert_eq!(receive.target_capacity(), 4);
        assert_eq!(receive.limits().capacity, 16);
        assert_eq!(receive.write_at(0, &&b"abcdefgh"[..]), 8);
        receive.make_readable(12, false);
        let mut out = [0; 12];
        assert_eq!(app.read(&mut out[..8]), 8);
        assert_eq!(receive.limits().capacity, 16);
        assert_eq!(app.read(&mut out[8..]), 4);
        assert_eq!(&out, b"abcdefghijkl");
        assert_eq!(receive.limits().capacity, 4);
        assert_eq!(receive.write_at(100, &&b"x"[..]), 0);
        assert_eq!(receive.limits().len, 0);
    }

    #[test]
    fn polling_unconnected_tcp_does_not_shutdown_future_connection() {
        use netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2 as V2;
        let runtime = Runtime::new(
            8,
            (0u8..=255).cycle().take(8192),
            NonZeroU64::new(1).unwrap(),
            [2, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        let mut provider = NativeSocketProvider::new(Rc::new(RefCell::new(runtime)));
        let client = SocketClientId::from_raw(43);
        V2::open_client(&mut provider, client, 2).unwrap();
        for family in [RemoteIpVersion::V4, RemoteIpVersion::V6] {
            let socket =
                V2::open_socket(&mut provider, client, ProviderSocketKindV2::Tcp, family).unwrap();
            for _ in 0..2 {
                let ready = V2::readiness(&mut provider, socket).unwrap();
                assert_eq!(
                    ready.readiness.0 & (Ready::READ_CLOSED | Ready::WRITE_CLOSED),
                    0,
                );
            }
        }
    }
}
