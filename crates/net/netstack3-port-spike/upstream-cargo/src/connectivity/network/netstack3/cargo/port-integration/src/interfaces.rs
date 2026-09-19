//! Bounded interface observations adapted from Fuchsia's interfaces, neighbor,
//! NDP and link-multicast workers. Protocol state remains in Netstack3 core.
//! Observations contain scalar IDs, never strong device references.

use crate::{NativeBindingsCtx, NativeInstant};
use net_types::{Witness, ethernet::Mac, ip::Ip};
use netstack3_core::{
    device::{DeviceId, EthernetDeviceEvent, EthernetDeviceId},
    ip::{IpAddressState, IpDeviceEvent, Lifetime, PreferredLifetime, RouterAdvertisementEvent},
    neighbor,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, net::IpAddr};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AddressState {
    Unavailable,
    Tentative,
    Assigned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PreferredUntil {
    Deprecated,
    Preferred(Option<u64>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Address {
    pub address: IpAddr,
    pub prefix: u8,
    pub state: AddressState,
    /// Nanoseconds in the provider's monotonic epoch; None means infinite.
    pub valid_until: Option<u64>,
    pub preferred_until: PreferredUntil,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NeighborState {
    Incomplete,
    Reachable,
    Stale,
    Delay,
    Probe,
    Unreachable,
    Static,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Neighbor {
    pub address: IpAddr,
    pub state: NeighborState,
    pub mac: Option<[u8; 6]>,
    pub observed_at: u64,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RouterAdvertisement {
    pub source: std::net::Ipv6Addr,
    pub observed_at: u64,
    pub options: Vec<u8>,
}

impl std::fmt::Debug for RouterAdvertisement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // NDP options may contain PII; raw observations are available only to
        // an explicit watcher, never implicitly in ordinary diagnostics.
        f.debug_struct("RouterAdvertisement")
            .field("source", &self.source)
            .field("observed_at", &self.observed_at)
            .field("options_len", &self.options.len())
            .finish()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct InterfaceSnapshot {
    pub id: u64,
    pub ipv4_enabled: bool,
    pub ipv6_enabled: bool,
    pub addresses: Vec<Address>,
    pub neighbors: Vec<Neighbor>,
    pub multicast: Vec<[u8; 6]>,
    pub router_advertisement: Option<RouterAdvertisement>,
    /// A bounded observer is not an unbounded history. Never silently claim a
    /// complete view after its address/neighbor/multicast/RA budget is exceeded.
    pub incomplete: bool,
}

#[derive(Default)]
pub(crate) struct Interfaces {
    snapshots: BTreeMap<u64, InterfaceSnapshot>,
    pub(crate) revision: u64,
}

fn ip<I: Ip>(addr: I::Addr) -> IpAddr {
    I::map_ip_in(
        addr,
        |a| IpAddr::V4(a.ipv4_bytes().into()),
        |a| IpAddr::V6(a.ipv6_bytes().into()),
    )
}
fn state(state: IpAddressState) -> AddressState {
    match state {
        IpAddressState::Unavailable => AddressState::Unavailable,
        IpAddressState::Tentative => AddressState::Tentative,
        IpAddressState::Assigned => AddressState::Assigned,
    }
}
fn lifetime(lifetime: Lifetime<NativeInstant>) -> Option<u64> {
    match lifetime {
        Lifetime::Infinite => None,
        Lifetime::Finite(at) => Some(at.as_nanos()),
    }
}
fn preferred(value: PreferredLifetime<NativeInstant>) -> PreferredUntil {
    match value {
        PreferredLifetime::Deprecated => PreferredUntil::Deprecated,
        PreferredLifetime::Preferred(until) => PreferredUntil::Preferred(lifetime(until)),
    }
}

impl Interfaces {
    /// Called only after core has accepted a device; address/configuration
    /// observations continue to arrive through typed core events.
    pub(crate) fn introduce(&mut self, id: u64) {
        self.device(id).expect("device admission exceeds interface observer capacity");
    }
    fn device(&mut self, id: u64) -> Option<&mut InterfaceSnapshot> {
        self.revision = self.revision.wrapping_add(1);
        if !self.snapshots.contains_key(&id) && self.snapshots.len() >= 64 {
            return None;
        }
        Some(
            self.snapshots
                .entry(id)
                .or_insert_with(|| InterfaceSnapshot {
                    id,
                    ..Default::default()
                }),
        )
    }
    pub(crate) fn snapshot(&self, id: u64) -> Option<InterfaceSnapshot> {
        self.snapshots.get(&id).cloned()
    }
    pub(crate) fn ip_event<I: Ip>(
        &mut self,
        event: IpDeviceEvent<DeviceId<NativeBindingsCtx>, I, NativeInstant>,
        limit: usize,
    ) {
        let limit = limit.min(64);
        // Apply synchronously, like the producer half of Fuchsia's interface
        // watcher. Coalescing snapshots cannot lose a removal behind a full
        // diagnostic queue or retain a removed core device.
        match event {
            IpDeviceEvent::AddressAdded {
                device,
                addr,
                state: assignment,
                valid_until,
                preferred_lifetime,
            } => {
                let Some(view) = self.device(device.bindings_id().0.get()) else {
                    return;
                };
                let address = ip::<I>(addr.addr().get());
                view.addresses.retain(|entry| entry.address != address);
                if view.addresses.len() == limit {
                    view.incomplete = true;
                    return;
                }
                view.addresses.push(Address {
                    address,
                    prefix: addr.subnet().prefix(),
                    state: state(assignment),
                    valid_until: lifetime(valid_until),
                    preferred_until: preferred(preferred_lifetime),
                });
            }
            IpDeviceEvent::AddressRemoved { device, addr, .. } => {
                if let Some(view) = self.device(device.bindings_id().0.get()) {
                    view.addresses
                        .retain(|entry| entry.address != ip::<I>(addr.get()));
                }
            }
            IpDeviceEvent::AddressStateChanged {
                device,
                addr,
                state: assignment,
            } => {
                if let Some(view) = self.device(device.bindings_id().0.get()) {
                    if let Some(entry) = view
                        .addresses
                        .iter_mut()
                        .find(|a| a.address == ip::<I>(addr.get()))
                    {
                        entry.state = state(assignment);
                    }
                }
            }
            IpDeviceEvent::AddressPropertiesChanged {
                device,
                addr,
                valid_until,
                preferred_lifetime,
            } => {
                if let Some(view) = self.device(device.bindings_id().0.get()) {
                    if let Some(entry) = view
                        .addresses
                        .iter_mut()
                        .find(|a| a.address == ip::<I>(addr.get()))
                    {
                        entry.valid_until = lifetime(valid_until);
                        entry.preferred_until = preferred(preferred_lifetime);
                    }
                }
            }
            IpDeviceEvent::EnabledChanged { device, ip_enabled } => {
                if let Some(view) = self.device(device.bindings_id().0.get()) {
                    match I::VERSION {
                        net_types::ip::IpVersion::V4 => view.ipv4_enabled = ip_enabled,
                        net_types::ip::IpVersion::V6 => view.ipv6_enabled = ip_enabled,
                    }
                }
            }
        }
    }

    pub(crate) fn neighbor_event<I: Ip>(
        &mut self,
        event: neighbor::Event<Mac, EthernetDeviceId<NativeBindingsCtx>, I, NativeInstant>,
        limit: usize,
    ) {
        let limit = limit.min(64);
        let Some(view) = self.device(event.device.bindings_id().0.get()) else {
            return;
        };
        let address = ip::<I>(event.addr.get());
        view.neighbors.retain(|entry| entry.address != address);
        let value = match event.kind {
            neighbor::EventKind::Removed => return,
            neighbor::EventKind::Added(v) | neighbor::EventKind::Changed(v) => v,
        };
        use neighbor::{EventDynamicState as D, EventState as S};
        let (state, mac) = match value {
            S::Static(mac) => (NeighborState::Static, Some(mac.get().bytes())),
            S::Dynamic(D::Incomplete) => (NeighborState::Incomplete, None),
            S::Dynamic(D::Reachable(mac)) => (NeighborState::Reachable, Some(mac.get().bytes())),
            S::Dynamic(D::Stale(mac)) => (NeighborState::Stale, Some(mac.get().bytes())),
            S::Dynamic(D::Delay(mac)) => (NeighborState::Delay, Some(mac.get().bytes())),
            S::Dynamic(D::Probe(mac)) => (NeighborState::Probe, Some(mac.get().bytes())),
            S::Dynamic(D::Unreachable(mac)) => {
                (NeighborState::Unreachable, Some(mac.get().bytes()))
            }
        };
        if view.neighbors.len() == limit {
            view.incomplete = true;
            return;
        }
        view.neighbors.push(Neighbor {
            address,
            state,
            mac,
            observed_at: event.at.as_nanos(),
        });
    }

    pub(crate) fn ra_event(
        &mut self,
        event: RouterAdvertisementEvent<DeviceId<NativeBindingsCtx>>,
        now: NativeInstant,
    ) {
        let Some(view) = self.device(event.device.bindings_id().0.get()) else {
            return;
        };
        // As in Fuchsia's bounded NDP watcher, overload loses observations,
        // never causes the protocol stack to discard its accepted RA state.
        if event.options_bytes.len() > 8192 {
            view.incomplete = true;
            return;
        }
        view.router_advertisement = Some(RouterAdvertisement {
            source: event.source.ipv6_bytes().into(),
            observed_at: now.as_nanos(),
            options: event.options_bytes.into_vec(),
        });
    }

    pub(crate) fn ethernet_event(
        &mut self,
        event: EthernetDeviceEvent<EthernetDeviceId<NativeBindingsCtx>>,
        limit: usize,
    ) {
        let limit = limit.min(64);
        match event {
            EthernetDeviceEvent::MulticastJoin { device, addr } => {
                let Some(view) = self.device(device.bindings_id().0.get()) else {
                    return;
                };
                let addr = addr.get().bytes();
                if !view.multicast.contains(&addr) {
                    if view.multicast.len() == limit {
                        view.incomplete = true;
                        return;
                    }
                    view.multicast.push(addr);
                }
            }
            EthernetDeviceEvent::MulticastLeave { device, addr } => {
                if let Some(view) = self.device(device.bindings_id().0.get()) {
                    view.multicast.retain(|entry| *entry != addr.get().bytes());
                }
            }
        }
    }
}
