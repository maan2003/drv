use alloc::vec::Vec;
use ath11k_wmi::event::{MgmtRx, MgmtTxCompletion, PeerStaKickout, Roam, Scan};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WlanEvent {
    ManagementReceived {
        pdev_id: u32,
        channel_mhz: u32,
        snr: u32,
        rssi: i32,
        flags: u32,
        frame: Vec<u8>,
    },
    ManagementTxCompleted {
        buffer_id: u32,
        status: u32,
        ack_rssi: u32,
    },
    Scan {
        event_type: u32,
        reason: u32,
        request_id: u32,
        scan_id: u32,
        vdev_id: u32,
        channel_mhz: u32,
    },
    BeaconLoss {
        vdev_id: u32,
        reason: u32,
        rssi: u32,
    },
    PeerKickout {
        peer: [u8; 6],
    },
}

impl From<MgmtRx> for WlanEvent {
    fn from(value: MgmtRx) -> Self {
        Self::ManagementReceived {
            pdev_id: value.pdev_id,
            channel_mhz: value.channel_freq,
            snr: value.snr,
            rssi: value.rssi,
            flags: value.flags,
            frame: value.frame,
        }
    }
}
impl From<MgmtTxCompletion> for WlanEvent {
    fn from(value: MgmtTxCompletion) -> Self {
        Self::ManagementTxCompleted {
            buffer_id: value.descriptor_id,
            status: value.status,
            ack_rssi: value.ack_rssi,
        }
    }
}
impl From<Scan> for WlanEvent {
    fn from(value: Scan) -> Self {
        Self::Scan {
            event_type: value.event_type,
            reason: value.reason,
            request_id: value.scan_request_id,
            scan_id: value.scan_id,
            vdev_id: value.vdev_id,
            channel_mhz: value.channel_freq,
        }
    }
}
impl TryFrom<Roam> for WlanEvent {
    type Error = crate::CoreError;
    fn try_from(value: Roam) -> Result<Self, Self::Error> {
        if value.reason != 2 {
            return Err(crate::CoreError::Protocol);
        }
        Ok(Self::BeaconLoss {
            vdev_id: value.vdev_id,
            reason: value.reason,
            rssi: value.rssi,
        })
    }
}
impl From<PeerStaKickout> for WlanEvent {
    fn from(value: PeerStaKickout) -> Self {
        Self::PeerKickout {
            peer: value.peer_mac,
        }
    }
}

/// Upward event half of the WlanSoftmac seam.
pub trait EventSink {
    fn handle(&mut self, event: WlanEvent);
}
