#![no_std]
#![forbid(unsafe_code)]
//! Device lifecycle and hardware-facing vdev/pdev/peer half of Linux mac.c.
use ath11k_qmi::FirmwareReady;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VdevId(pub u8);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdevId(pub u8);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreError {
    WrongState,
    Protocol,
    DeviceFault,
}
pub trait Lifecycle {
    fn probe(&mut self) -> Result<(), CoreError>;
    fn attach_firmware(&mut self, ready: FirmwareReady) -> Result<(), CoreError>;
    fn start_radio(&mut self) -> Result<(), CoreError>;
    fn stop(&mut self) -> Result<(), CoreError>;
}
/// Hardware effects behind the project WlanSoftmac seam. MLME policy and
/// mac80211 callbacks are replaced rather than ported.
pub trait RadioControl {
    fn create_client_vdev(&mut self, mac: [u8; 6]) -> Result<VdevId, CoreError>;
    fn start_vdev(&mut self, vdev: VdevId, frequency_mhz: u16) -> Result<(), CoreError>;
    fn create_peer(&mut self, vdev: VdevId, address: [u8; 6]) -> Result<(), CoreError>;
    fn delete_peer(&mut self, vdev: VdevId, address: [u8; 6]) -> Result<(), CoreError>;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_are_typed() {
        let _v = VdevId(1);
        let _p = PdevId(1);
    }
}
