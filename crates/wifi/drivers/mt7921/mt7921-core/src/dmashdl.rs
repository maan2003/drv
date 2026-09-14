const WFDMA_TX_DMASHDL_ENABLE: u32 = 1 << 6;
const DMASHDL_BYPASS: u32 = 1 << 28;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DmashdlInvariantReadback {
    pub ext0_before: u32,
    pub control_before: u32,
    pub ext0_after: u32,
    pub control_after: u32,
    pub attempts: u8,
}

pub trait DmashdlInvariantIo {
    type Error;

    fn read_ext0(&mut self) -> Result<u32, Self::Error>;
    fn write_ext0(&mut self, value: u32) -> Result<(), Self::Error>;
    fn read_control(&mut self) -> Result<u32, Self::Error>;
    fn write_control(&mut self, value: u32) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DmashdlInvariantError<E> {
    Io(E),
    ReadAllOnes,
    ReadbackAllOnes,
    DidNotLatch { ext0: u32, control: u32 },
}

impl<E: core::fmt::Display> core::fmt::Display for DmashdlInvariantError<E> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::ReadAllOnes => formatter.write_str("DMASHDL invariant read returned all ones"),
            Self::ReadbackAllOnes => {
                formatter.write_str("DMASHDL invariant readback returned all ones")
            }
            Self::DidNotLatch { ext0, control } => write!(
                formatter,
                "DMASHDL invariant did not latch ext0={ext0:#010x} control={control:#010x}"
            ),
        }
    }
}

pub fn ensure_linux_dmashdl_invariant<I: DmashdlInvariantIo>(
    io: &mut I,
) -> Result<DmashdlInvariantReadback, DmashdlInvariantError<I::Error>> {
    let ext0_before = io.read_ext0().map_err(DmashdlInvariantError::Io)?;
    let control_before = io.read_control().map_err(DmashdlInvariantError::Io)?;
    if ext0_before == u32::MAX || control_before == u32::MAX {
        return Err(DmashdlInvariantError::ReadAllOnes);
    }
    let mut ext0_after = ext0_before;
    let mut control_after = control_before;
    let mut attempts = 0;
    while ext0_after & WFDMA_TX_DMASHDL_ENABLE != 0 || control_after & DMASHDL_BYPASS == 0 {
        if attempts == 2 {
            return Err(DmashdlInvariantError::DidNotLatch {
                ext0: ext0_after,
                control: control_after,
            });
        }
        attempts += 1;
        if ext0_after & WFDMA_TX_DMASHDL_ENABLE != 0 {
            io.write_ext0(ext0_after & !WFDMA_TX_DMASHDL_ENABLE)
                .map_err(DmashdlInvariantError::Io)?;
        }
        if control_after & DMASHDL_BYPASS == 0 {
            io.write_control(control_after | DMASHDL_BYPASS)
                .map_err(DmashdlInvariantError::Io)?;
        }
        // A BAR read flushes the preceding posted PCIe write. Linux performs
        // this idempotent RMW while DMA is disabled; one retry is safe at the
        // same lifecycle point and catches an unlatched first publication.
        ext0_after = io.read_ext0().map_err(DmashdlInvariantError::Io)?;
        control_after = io.read_control().map_err(DmashdlInvariantError::Io)?;
        if ext0_after == u32::MAX || control_after == u32::MAX {
            return Err(DmashdlInvariantError::ReadbackAllOnes);
        }
    }
    Ok(DmashdlInvariantReadback {
        ext0_before,
        control_before,
        ext0_after,
        control_after,
        attempts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeDmashdl {
        ext0: u32,
        control: u32,
        drop_first_control_write: bool,
        ext0_writes: usize,
        control_writes: usize,
    }

    impl DmashdlInvariantIo for FakeDmashdl {
        type Error = &'static str;

        fn read_ext0(&mut self) -> Result<u32, Self::Error> {
            Ok(self.ext0)
        }

        fn write_ext0(&mut self, value: u32) -> Result<(), Self::Error> {
            self.ext0_writes += 1;
            self.ext0 = value;
            Ok(())
        }

        fn read_control(&mut self) -> Result<u32, Self::Error> {
            Ok(self.control)
        }

        fn write_control(&mut self, value: u32) -> Result<(), Self::Error> {
            self.control_writes += 1;
            if self.drop_first_control_write {
                self.drop_first_control_write = false;
            } else {
                self.control = value;
            }
            Ok(())
        }
    }

    #[test]
    fn invariant_retries_and_reasserts_after_reset() {
        let mut lost_once = FakeDmashdl {
            ext0: WFDMA_TX_DMASHDL_ENABLE | 3,
            control: 5,
            drop_first_control_write: true,
            ext0_writes: 0,
            control_writes: 0,
        };
        let readback = ensure_linux_dmashdl_invariant(&mut lost_once).unwrap();
        assert_eq!(readback.attempts, 2);
        assert_eq!(readback.ext0_after, 3);
        assert_eq!(readback.control_after, DMASHDL_BYPASS | 5);
        assert_eq!(lost_once.ext0_writes, 1);
        assert_eq!(lost_once.control_writes, 2);

        let mut correct = FakeDmashdl {
            ext0: 3,
            control: DMASHDL_BYPASS | 5,
            drop_first_control_write: false,
            ext0_writes: 0,
            control_writes: 0,
        };
        let readback = ensure_linux_dmashdl_invariant(&mut correct).unwrap();
        assert_eq!(readback.attempts, 0);
        assert_eq!((correct.ext0_writes, correct.control_writes), (0, 0));

        correct.control &= !DMASHDL_BYPASS;
        assert_eq!(correct.control & DMASHDL_BYPASS, 0);
        let readback = ensure_linux_dmashdl_invariant(&mut correct).unwrap();
        assert_eq!(readback.control_before & DMASHDL_BYPASS, 0);
        assert_ne!(readback.control_after & DMASHDL_BYPASS, 0);
        assert_eq!(correct.control_writes, 1);
    }
}
