// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host execution of the chip-independent SoftMAC contract and pinned WLAN
//! protocol. Device contracts live in `wlan-softmac-class-support`; this crate
//! owns mailbox scheduling, protocol execution and Ethernet service bindings.

mod driver;
pub mod ethernet;
pub mod runtime;

#[cfg(any(test, feature = "conformance"))]
pub use wlan_softmac_class_support::conformance;
pub use wlan_softmac_class_support::*;
