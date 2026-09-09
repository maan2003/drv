use crate::{
    ClcSetResponse, ClcSetResponseError, DownloadCommand, EepromBlockError,
    FirmwareCommandCompletion, NicCapabilityError, parse_clc_set_response, parse_eeprom_block,
    parse_nic_capability,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McuResponse<'a> {
    pub event_id: u8,
    pub option: u8,
    pub bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McuCompletionError {
    PatchSemaphoreEvent(u8),
    PatchSemaphoreResultOmitted,
    PatchFinishStatusOmitted,
    NicCapabilityHeaderOmitted,
    NicCapability(NicCapabilityError),
    EepromHeaderOmitted,
    Eeprom(EepromBlockError),
    UnexpectedNoResponseCommand,
}

impl core::fmt::Display for McuCompletionError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PatchSemaphoreEvent(event) => write!(
                formatter,
                "patch semaphore response event was {event:#04x}, expected 0x04"
            ),
            Self::PatchSemaphoreResultOmitted => {
                formatter.write_str("patch semaphore response omitted result")
            }
            Self::PatchFinishStatusOmitted => {
                formatter.write_str("patch finish response omitted status")
            }
            Self::NicCapabilityHeaderOmitted => {
                formatter.write_str("NIC capability response omitted MCU header")
            }
            Self::NicCapability(error) => {
                write!(formatter, "parse NIC capability response: {error:?}")
            }
            Self::EepromHeaderOmitted => formatter.write_str("EEPROM response omitted MCU header"),
            Self::Eeprom(error) => write!(formatter, "parse EEPROM response: {error:?}"),
            Self::UnexpectedNoResponseCommand => {
                formatter.write_str("no-response command unexpectedly requested RX classification")
            }
        }
    }
}

pub fn classify_mcu_completion(
    command: DownloadCommand,
    response: McuResponse<'_>,
) -> Result<FirmwareCommandCompletion, McuCompletionError> {
    match command {
        DownloadCommand::PatchSemaphoreGet | DownloadCommand::PatchSemaphoreRelease => {
            if response.event_id != 0x04 {
                return Err(McuCompletionError::PatchSemaphoreEvent(response.event_id));
            }
            let result = response
                .bytes
                .get(32)
                .copied()
                .ok_or(McuCompletionError::PatchSemaphoreResultOmitted)?;
            Ok(FirmwareCommandCompletion::PatchSemaphore(result.into()))
        }
        DownloadCommand::PatchFinish => {
            let status = response
                .bytes
                .get(32)
                .copied()
                .ok_or(McuCompletionError::PatchFinishStatusOmitted)?;
            Ok(FirmwareCommandCompletion::PatchFinish(status))
        }
        DownloadCommand::PatchStart { .. }
        | DownloadCommand::TargetAddressLength { .. }
        | DownloadCommand::EepromBufferMode
        | DownloadCommand::ProtectControl
        | DownloadCommand::FirmwareStart { .. } => Ok(FirmwareCommandCompletion::Ack),
        DownloadCommand::GetNicCapability => {
            let body = response
                .bytes
                .get(36..)
                .ok_or(McuCompletionError::NicCapabilityHeaderOmitted)?;
            let capability =
                parse_nic_capability(body).map_err(McuCompletionError::NicCapability)?;
            Ok(FirmwareCommandCompletion::NicCapability(capability))
        }
        DownloadCommand::ReadEepromBlock { address } => {
            let body = response
                .bytes
                .get(36..)
                .ok_or(McuCompletionError::EepromHeaderOmitted)?;
            let block = parse_eeprom_block(body, address).map_err(McuCompletionError::Eeprom)?;
            Ok(FirmwareCommandCompletion::EepromBlock(block))
        }
        DownloadCommand::NicPowerControl | DownloadCommand::FirmwareLogToHost => {
            Err(McuCompletionError::UnexpectedNoResponseCommand)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClcResponseError {
    Event(u8),
    Unsolicited,
    HeaderOmitted,
    Parse(ClcSetResponseError),
}

impl core::fmt::Display for ClcResponseError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Event(event) => write!(
                formatter,
                "SET_CLC response event was {event:#04x}, expected 0x80"
            ),
            Self::Unsolicited => {
                formatter.write_str("SET_CLC response was marked as an unsolicited event")
            }
            Self::HeaderOmitted => formatter.write_str("SET_CLC response omitted MCU header"),
            Self::Parse(error) => write!(formatter, "parse SET_CLC response: {error:?}"),
        }
    }
}

pub fn classify_clc_response(
    response: McuResponse<'_>,
) -> Result<ClcSetResponse, ClcResponseError> {
    if response.event_id != 0x80 {
        return Err(ClcResponseError::Event(response.event_id));
    }
    if response.option & (1 << 2) != 0 {
        return Err(ClcResponseError::Unsolicited);
    }
    let body = response
        .bytes
        .get(36..)
        .ok_or(ClcResponseError::HeaderOmitted)?;
    parse_clc_set_response(body).map_err(ClcResponseError::Parse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EepromBlock, NicCapability, PatchSemaphoreStatus};
    use alloc::vec;

    #[test]
    fn classifies_firmware_and_clc_responses_fail_closed() {
        let mut bytes = vec![0; 33];
        bytes[32] = 2;
        assert_eq!(
            classify_mcu_completion(
                DownloadCommand::PatchSemaphoreGet,
                McuResponse {
                    event_id: 0x04,
                    option: 0,
                    bytes: &bytes,
                },
            ),
            Ok(FirmwareCommandCompletion::PatchSemaphore(
                PatchSemaphoreStatus::Acquired
            ))
        );
        assert_eq!(
            classify_mcu_completion(
                DownloadCommand::PatchSemaphoreGet,
                McuResponse {
                    event_id: 3,
                    option: 0,
                    bytes: &bytes,
                },
            ),
            Err(McuCompletionError::PatchSemaphoreEvent(3))
        );
        assert!(
            classify_mcu_completion(
                DownloadCommand::PatchSemaphoreGet,
                McuResponse {
                    event_id: 4,
                    option: 0,
                    bytes: &[0; 32],
                },
            )
            .is_err()
        );

        let capability_bytes = vec![0; 40];
        assert_eq!(
            classify_mcu_completion(
                DownloadCommand::GetNicCapability,
                McuResponse {
                    event_id: 1,
                    option: 0,
                    bytes: &capability_bytes,
                },
            ),
            Ok(FirmwareCommandCompletion::NicCapability(NicCapability {
                element_count: 0,
                mac_address: None,
                phy: None,
                has_6ghz: None,
                chip_capability: None,
                unknown_elements: 0,
            }))
        );
        assert!(
            classify_mcu_completion(
                DownloadCommand::GetNicCapability,
                McuResponse {
                    event_id: 1,
                    option: 0,
                    bytes: &[0; 39],
                },
            )
            .is_err()
        );

        let mut eeprom_bytes = vec![0; 60];
        eeprom_bytes[36..40].copy_from_slice(&0x550u32.to_le_bytes());
        eeprom_bytes[40..44].copy_from_slice(&1u32.to_le_bytes());
        eeprom_bytes[55] = 1;
        assert_eq!(
            classify_mcu_completion(
                DownloadCommand::ReadEepromBlock { address: 0x550 },
                McuResponse {
                    event_id: 1,
                    option: 0,
                    bytes: &eeprom_bytes,
                },
            ),
            Ok(FirmwareCommandCompletion::EepromBlock(EepromBlock {
                address: 0x550,
                valid: 1,
                data: {
                    let mut data = [0; 16];
                    data[11] = 1;
                    data
                },
            }))
        );

        let mut clc_bytes = vec![0; 108];
        clc_bytes[42..44].copy_from_slice(&68u16.to_le_bytes());
        clc_bytes[44] = 0x1f;
        assert_eq!(
            classify_clc_response(McuResponse {
                event_id: 0x80,
                option: 0,
                bytes: &clc_bytes,
            }),
            Ok(ClcSetResponse {
                tag: 0,
                length: 68,
                special_unii_mask: 0x1f,
            })
        );
        assert_eq!(
            classify_clc_response(McuResponse {
                event_id: 0x80,
                option: 1 << 2,
                bytes: &clc_bytes,
            }),
            Err(ClcResponseError::Unsolicited)
        );
    }
}
