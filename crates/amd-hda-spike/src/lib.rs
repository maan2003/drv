#![forbid(unsafe_code)]

use std::{
    thread,
    time::{Duration, Instant},
};

pub const EXPECTED_CODEC_VENDOR: u32 = 0x10ec_0256;
const RIRB_OFFSET: usize = 1024;
const BDL_OFFSET: usize = 4096;
const PCM_OFFSET: usize = 8192;
const PCM_BYTES: usize = 7680;

pub trait Transport {
    type Error;
    fn read8(&self, offset: usize) -> Result<u8, Self::Error>;
    fn read16(&self, offset: usize) -> Result<u16, Self::Error>;
    fn read32(&self, offset: usize) -> Result<u32, Self::Error>;
    fn write8(&mut self, offset: usize, value: u8) -> Result<(), Self::Error>;
    fn write16(&mut self, offset: usize, value: u16) -> Result<(), Self::Error>;
    fn write32(&mut self, offset: usize, value: u32) -> Result<(), Self::Error>;
    fn dma_iova(&self) -> u64;
    fn dma_write32(&mut self, offset: usize, value: u32);
    fn dma_read32(&self, offset: usize) -> u32;
    fn fence(&self);
    fn take_irq_count(&mut self) -> Result<u64, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Widget {
    pub node: u8,
    pub capabilities: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecInventory {
    pub address: u8,
    pub vendor_device: u32,
    pub revision: u32,
    pub function_groups: Vec<u8>,
    pub widgets: Vec<Widget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputRoute {
    Headphone,
    Speaker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackReport {
    pub route: OutputRoute,
    pub stream_index: u8,
    pub start_position: u32,
    pub end_position: u32,
    pub irq_count: u64,
    pub amp_gain_step: u8,
}

#[derive(Debug)]
pub enum Error<E> {
    Io(E),
    Timeout(&'static str),
    Unsupported(&'static str),
    UnexpectedCodec(u32),
}

fn map_io<R, E>(value: Result<R, E>) -> Result<R, Error<E>> {
    value.map_err(Error::Io)
}

fn retain_first_error<E>(result: &mut Result<(), E>, next: Result<(), E>) {
    if result.is_ok() {
        *result = next;
    }
}

pub struct Controller<T: Transport> {
    io: T,
    corb_wp: u16,
    rirb_rp: u16,
}

impl<T: Transport> Controller<T> {
    pub fn new(io: T) -> Self {
        Self {
            io,
            corb_wp: 0,
            rirb_rp: 0,
        }
    }
    fn poll(
        &self,
        what: &'static str,
        mut ready: impl FnMut(&T) -> Result<bool, T::Error>,
    ) -> Result<(), Error<T::Error>> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if map_io(ready(&self.io))? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout(what));
            }
            thread::sleep(Duration::from_micros(10));
        }
    }
    pub fn reset(&mut self) -> Result<u16, Error<T::Error>> {
        map_io(self.io.write32(0x20, 0))?;
        let gcap = map_io(self.io.read16(0x00))?;
        let streams = ((gcap >> 8) & 0x0f) + ((gcap >> 12) & 0x0f) + ((gcap >> 3) & 0x1f);
        if streams > 30 {
            return Err(Error::Unsupported("invalid stream count"));
        }
        for index in 0..streams as usize {
            let base = 0x80 + index * 0x20;
            let ctl = map_io(self.io.read32(base))? & 0x00ff_ffff;
            map_io(self.io.write32(base, ctl & !0x02))?;
            self.poll("stop stream", |io| Ok(io.read32(base)? & 0x02 == 0))?;
            map_io(self.io.write32(base, (ctl & !0x02) | 0x01))?;
            self.poll("assert stream reset", |io| Ok(io.read32(base)? & 0x01 != 0))?;
            map_io(self.io.write32(base, ctl & !0x03))?;
            self.poll("deassert stream reset", |io| {
                Ok(io.read32(base)? & 0x01 == 0)
            })?;
        }
        map_io(self.io.write8(0x4c, 0))?;
        map_io(self.io.write8(0x5c, 0))?;
        let gctl = map_io(self.io.read32(0x08))?;
        map_io(self.io.write32(0x08, gctl & !1))?;
        self.poll("enter GCTL reset", |io| Ok(io.read32(0x08)? & 1 == 0))?;
        thread::sleep(Duration::from_micros(100));
        map_io(self.io.write32(0x08, gctl | 1))?;
        self.poll("leave GCTL reset", |io| Ok(io.read32(0x08)? & 1 != 0))?;
        thread::sleep(Duration::from_micros(521));
        let major = map_io(self.io.read8(0x03))?;
        let minor = map_io(self.io.read8(0x02))?;
        if (major, minor) != (1, 0) {
            return Err(Error::Unsupported("HDA version is not 1.0"));
        }
        map_io(self.io.read16(0x0e))
    }
    pub fn start_command_rings(&mut self) -> Result<(), Error<T::Error>> {
        let corb_size = map_io(self.io.read8(0x4e))?;
        let rirb_size = map_io(self.io.read8(0x5e))?;
        if corb_size & 0x40 == 0 || rirb_size & 0x40 == 0 {
            return Err(Error::Unsupported("controller lacks 256-entry CORB/RIRB"));
        }
        let iova = self.io.dma_iova();
        map_io(self.io.write32(0x40, iova as u32))?;
        map_io(self.io.write32(0x44, (iova >> 32) as u32))?;
        let rirb = iova + RIRB_OFFSET as u64;
        map_io(self.io.write32(0x50, rirb as u32))?;
        map_io(self.io.write32(0x54, (rirb >> 32) as u32))?;
        map_io(self.io.write8(0x4e, 2))?;
        map_io(self.io.write8(0x5e, 2))?;
        map_io(self.io.write16(0x48, 0))?;
        map_io(self.io.write16(0x4a, 0x8000))?;
        self.poll("set CORB read-pointer reset", |io| {
            Ok(io.read16(0x4a)? & 0x8000 != 0)
        })?;
        map_io(self.io.write16(0x4a, 0))?;
        self.poll("clear CORB read-pointer reset", |io| {
            Ok(io.read16(0x4a)? & 0x8000 == 0)
        })?;
        map_io(self.io.write16(0x58, 0x8000))?;
        map_io(self.io.write16(0x5a, 1))?;
        map_io(self.io.write8(0x4d, 0xff))?;
        map_io(self.io.write8(0x5d, 0xff))?;
        map_io(self.io.write8(0x4c, 0x02))?;
        map_io(self.io.write8(0x5c, 0x03))?;
        // Global interrupt enable + controller interrupt enable. VFIO owns the
        // sole MSI eventfd; command completion still polls RIRBWP so recovery
        // never depends on userspace interrupt scheduling.
        map_io(self.io.write32(0x20, 0xc000_0000))?;
        self.corb_wp = 0;
        self.rirb_rp = 0;
        Ok(())
    }
    pub fn command(
        &mut self,
        codec: u8,
        node: u8,
        verb: u16,
        payload: u8,
    ) -> Result<u32, Error<T::Error>> {
        self.command_word(codec, node, ((verb as u32) << 8) | payload as u32)
    }
    fn command_long(
        &mut self,
        codec: u8,
        node: u8,
        verb: u8,
        payload: u16,
    ) -> Result<u32, Error<T::Error>> {
        self.command_word(codec, node, ((verb as u32) << 16) | payload as u32)
    }
    fn command_word(&mut self, codec: u8, node: u8, lower_20: u32) -> Result<u32, Error<T::Error>> {
        self.corb_wp = (self.corb_wp + 1) & 0xff;
        let command = ((codec as u32) << 28) | ((node as u32) << 20) | lower_20;
        self.io.dma_write32(self.corb_wp as usize * 4, command);
        self.io.fence();
        map_io(self.io.write16(0x48, self.corb_wp))?;
        let expected = (self.rirb_rp + 1) & 0xff;
        self.poll(
            "RIRB response",
            |io| Ok(io.read16(0x58)? & 0xff == expected),
        )?;
        self.io.fence();
        let response = self.io.dma_read32(RIRB_OFFSET + expected as usize * 8);
        let response_ex = self.io.dma_read32(RIRB_OFFSET + expected as usize * 8 + 4);
        self.rirb_rp = expected;
        map_io(self.io.write8(0x5d, 0x01))?;
        if (response_ex & 0x0f) as u8 != codec {
            return Err(Error::Unsupported("RIRB codec address mismatch"));
        }
        Ok(response)
    }
    fn parameter(&mut self, codec: u8, node: u8, parameter: u8) -> Result<u32, Error<T::Error>> {
        self.command(codec, node, 0x0f00, parameter)
    }
    pub fn enumerate_codec(&mut self, address: u8) -> Result<CodecInventory, Error<T::Error>> {
        let vendor_device = self.parameter(address, 0, 0x00)?;
        if vendor_device != EXPECTED_CODEC_VENDOR {
            return Err(Error::UnexpectedCodec(vendor_device));
        }
        let revision = self.parameter(address, 0, 0x02)?;
        let fg_nodes = self.parameter(address, 0, 0x04)?;
        let fg_start = (fg_nodes >> 16) as u8;
        let fg_count = fg_nodes as u8;
        let mut function_groups = Vec::new();
        let mut widgets = Vec::new();
        for fg in fg_start..fg_start.saturating_add(fg_count) {
            function_groups.push(fg);
            let nodes = self.parameter(address, fg, 0x04)?;
            let start = (nodes >> 16) as u8;
            let count = nodes as u8;
            for node in start..start.saturating_add(count) {
                widgets.push(Widget {
                    node,
                    capabilities: self.parameter(address, node, 0x09)?,
                });
            }
        }
        Ok(CodecInventory {
            address,
            vendor_device,
            revision,
            function_groups,
            widgets,
        })
    }
    pub fn play_pcm_period(
        &mut self,
        codec: u8,
        pcm: &[u8],
    ) -> Result<PlaybackReport, Error<T::Error>> {
        if pcm.is_empty() || pcm.len() > PCM_BYTES || !pcm.len().is_multiple_of(4) {
            return Err(Error::Unsupported(
                "PCM period must be 1..7680 bytes of stereo S16LE",
            ));
        }
        let gcap = map_io(self.io.read16(0x00))?;
        let stream_index = ((gcap >> 8) & 0x0f) as u8;
        if (gcap >> 12) & 0x0f == 0 {
            return Err(Error::Unsupported("controller has no output stream"));
        }
        let stream = 0x80 + stream_index as usize * 0x20;
        let (route, pin) = if self.command(codec, 0x21, 0x0f09, 0)? & (1 << 31) != 0 {
            (OutputRoute::Headphone, 0x21)
        } else {
            (OutputRoute::Speaker, 0x14)
        };

        let amp_caps = self.parameter(codec, 0x02, 0x12)?;
        let offset = (amp_caps & 0x7f) as u8;
        let quarter_db = (((amp_caps >> 16) & 0x7f) + 1) as u8;
        let attenuation_steps = 144_u16.div_ceil(u16::from(quarter_db.max(1))) as u8;
        let gain = offset.saturating_sub(attenuation_steps); // approximately -36 dB

        let ctl = (1 << 20) | (1 << 19);
        let playback = (|| {
            for node in [0x01, 0x02, pin] {
                self.command(codec, node, 0x0705, 0)?;
            }
            thread::sleep(Duration::from_millis(2));
            self.command_long(codec, 0x02, 0x3, 0xb080)?;
            self.command_long(codec, pin, 0x3, 0xb080)?;
            self.command(codec, pin, 0x0701, 0)?;
            self.command_long(codec, 0x02, 0x2, 0x0011)?;
            self.command(codec, 0x02, 0x0706, 0x10)?;
            let pin_ctl = if route == OutputRoute::Headphone {
                0xc0
            } else {
                0x40
            };
            self.command(codec, pin, 0x0707, pin_ctl)?;
            self.command(codec, pin, 0x070c, 0x02)?;

            for (index, word) in pcm.chunks_exact(4).enumerate() {
                self.io.dma_write32(
                    PCM_OFFSET + index * 4,
                    u32::from_le_bytes(word.try_into().unwrap()),
                );
            }
            let pcm_iova = self.io.dma_iova() + PCM_OFFSET as u64;
            self.io.dma_write32(BDL_OFFSET, pcm_iova as u32);
            self.io.dma_write32(BDL_OFFSET + 4, (pcm_iova >> 32) as u32);
            self.io.dma_write32(BDL_OFFSET + 8, pcm.len() as u32);
            self.io.dma_write32(BDL_OFFSET + 12, 1);
            self.io.fence();

            map_io(self.io.write32(stream, ctl))?;
            map_io(self.io.write16(stream + 0x12, 0x0011))?;
            let bdl_iova = self.io.dma_iova() + BDL_OFFSET as u64;
            map_io(self.io.write32(stream + 0x18, bdl_iova as u32))?;
            map_io(self.io.write32(stream + 0x1c, (bdl_iova >> 32) as u32))?;
            map_io(self.io.write32(stream + 0x08, pcm.len() as u32))?;
            map_io(self.io.write16(stream + 0x0c, 0))?;
            map_io(self.io.write8(stream + 0x03, 0x1c))?;
            map_io(self.io.write32(0x20, 0xc000_0000 | (1 << stream_index)))?;
            self.command_long(codec, 0x02, 0x3, 0xb000 | u16::from(gain))?;
            self.command_long(codec, pin, 0x3, 0xb000)?;
            // Drain command-response MSI counts before RUN. Any later eventfd
            // count therefore comes from the stream IOC entry, not a codec verb.
            while map_io(self.io.take_irq_count())? != 0 {}
            let start_position = map_io(self.io.read32(stream + 0x04))?;
            map_io(self.io.write32(stream, ctl | 0x1e))?;
            self.io.fence();

            let deadline = Instant::now() + Duration::from_millis(250);
            let mut irq_count = 0;
            let mut end_position = start_position;
            let mut position_changed = false;
            let mut stream_fault = false;
            while Instant::now() < deadline {
                irq_count += map_io(self.io.take_irq_count())?;
                let position = map_io(self.io.read32(stream + 0x04))?;
                if position != start_position {
                    end_position = position;
                    position_changed = true;
                }
                if map_io(self.io.read8(stream + 0x03))? & 0x18 != 0 {
                    stream_fault = true;
                    break;
                }
                if irq_count != 0 && position_changed {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            if stream_fault {
                return Err(Error::Unsupported("stream FIFO/descriptor error"));
            }
            if irq_count == 0 || !position_changed {
                return Err(Error::Timeout("stream MSI/position completion"));
            }
            Ok(PlaybackReport {
                route,
                stream_index,
                start_position,
                end_position,
                irq_count,
                amp_gain_step: gain,
            })
        })();

        // Once route setup begins, quiesce it on every exit. In particular,
        // eventfd/MMIO errors after unmute must not leave EAPD or DMA active.
        let cleanup = self.quiesce_playback(codec, pin, stream, ctl);
        match playback {
            Err(error) => Err(error),
            Ok(report) => cleanup.map(|()| report),
        }
    }
    fn quiesce_playback(
        &mut self,
        codec: u8,
        pin: u8,
        stream: usize,
        ctl: u32,
    ) -> Result<(), Error<T::Error>> {
        let mut result = Ok(());
        retain_first_error(
            &mut result,
            self.command_long(codec, 0x02, 0x3, 0xb080).map(|_| ()),
        );
        retain_first_error(
            &mut result,
            self.command_long(codec, pin, 0x3, 0xb080).map(|_| ()),
        );
        match map_io(self.io.write32(stream, ctl)) {
            Ok(()) => retain_first_error(
                &mut result,
                self.poll("stop playback stream", |io| Ok(io.read32(stream)? & 2 == 0)),
            ),
            Err(error) => retain_first_error(&mut result, Err(error)),
        }
        match map_io(self.io.write32(stream, ctl | 1)) {
            Ok(()) => retain_first_error(
                &mut result,
                self.poll("assert playback stream reset", |io| {
                    Ok(io.read32(stream)? & 1 != 0)
                }),
            ),
            Err(error) => retain_first_error(&mut result, Err(error)),
        }
        match map_io(self.io.write32(stream, 0)) {
            Ok(()) => retain_first_error(
                &mut result,
                self.poll("deassert playback stream reset", |io| {
                    Ok(io.read32(stream)? & 1 == 0)
                }),
            ),
            Err(error) => retain_first_error(&mut result, Err(error)),
        }
        retain_first_error(
            &mut result,
            self.command(codec, 0x02, 0x0706, 0).map(|_| ()),
        );
        retain_first_error(&mut result, self.command(codec, pin, 0x0707, 0).map(|_| ()));
        retain_first_error(&mut result, self.command(codec, pin, 0x070c, 0).map(|_| ()));
        result
    }
    pub fn shutdown(&mut self) {
        let _ = self.io.write32(0x20, 0);
        let _ = self.io.write8(0x4c, 0);
        let _ = self.io.write8(0x5c, 0);
        self.io.fence();
    }
}

impl<T: Transport> Drop for Controller<T> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        regs: RefCell<Vec<u8>>,
        dma: RefCell<Vec<u8>>,
        commands: RefCell<Vec<u32>>,
        irq: u64,
        fail_irq: bool,
    }
    impl Fake {
        fn new() -> Self {
            let mut regs = vec![0; 0x8000];
            regs[0..2].copy_from_slice(&0x1101_u16.to_le_bytes()); // one input, one output
            regs[2] = 0;
            regs[3] = 1;
            regs[8..12].copy_from_slice(&1_u32.to_le_bytes());
            regs[0x0e..0x10].copy_from_slice(&1_u16.to_le_bytes());
            regs[0x4e] = 0x40;
            regs[0x5e] = 0x40;
            Self {
                regs: RefCell::new(regs),
                dma: RefCell::new(vec![0; 16 * 1024]),
                commands: RefCell::new(Vec::new()),
                irq: 0,
                fail_irq: false,
            }
        }
        fn response(command: u32) -> u32 {
            let node = ((command >> 20) & 0xff) as u8;
            let parameter = command as u8;
            match (node, parameter) {
                (0, 0x00) => EXPECTED_CODEC_VENDOR,
                (0, 0x02) => 0x0010_0101,
                (0, 0x04) => 0x0001_0001,
                (1, 0x04) => 0x0002_0002,
                (2, 0x09) => 0x0000_0001,
                (2, 0x12) => 0x8002_7f7f,
                (3, 0x09) => 0x0040_0001,
                _ => 0,
            }
        }
    }
    impl Transport for Fake {
        type Error = ();
        fn read8(&self, o: usize) -> Result<u8, ()> {
            Ok(self.regs.borrow()[o])
        }
        fn read16(&self, o: usize) -> Result<u16, ()> {
            Ok(u16::from_le_bytes(
                self.regs.borrow()[o..o + 2].try_into().unwrap(),
            ))
        }
        fn read32(&self, o: usize) -> Result<u32, ()> {
            Ok(u32::from_le_bytes(
                self.regs.borrow()[o..o + 4].try_into().unwrap(),
            ))
        }
        fn write8(&mut self, o: usize, v: u8) -> Result<(), ()> {
            self.regs.borrow_mut()[o] = v;
            Ok(())
        }
        fn write16(&mut self, o: usize, v: u16) -> Result<(), ()> {
            self.regs.borrow_mut()[o..o + 2].copy_from_slice(&v.to_le_bytes());
            if o == 0x48 {
                let command = self.dma_read32(v as usize * 4);
                self.commands.borrow_mut().push(command);
                let response = Self::response(command);
                let next = ((self.read16(0x58)? & 0xff) + 1) & 0xff;
                self.dma.borrow_mut()
                    [RIRB_OFFSET + next as usize * 8..RIRB_OFFSET + next as usize * 8 + 4]
                    .copy_from_slice(&response.to_le_bytes());
                self.regs.borrow_mut()[0x58..0x5a].copy_from_slice(&next.to_le_bytes());
            }
            Ok(())
        }
        fn write32(&mut self, o: usize, v: u32) -> Result<(), ()> {
            self.regs.borrow_mut()[o..o + 4].copy_from_slice(&v.to_le_bytes());
            if o == 0xa0 && v & 2 != 0 {
                self.regs.borrow_mut()[o + 4..o + 8].copy_from_slice(&128_u32.to_le_bytes());
                self.regs.borrow_mut()[o + 3] = 4;
                self.irq += 1;
            }
            Ok(())
        }
        fn dma_iova(&self) -> u64 {
            0x1000_0000
        }
        fn dma_write32(&mut self, o: usize, v: u32) {
            self.dma.borrow_mut()[o..o + 4].copy_from_slice(&v.to_le_bytes())
        }
        fn dma_read32(&self, o: usize) -> u32 {
            u32::from_le_bytes(self.dma.borrow()[o..o + 4].try_into().unwrap())
        }
        fn fence(&self) {}
        fn take_irq_count(&mut self) -> Result<u64, ()> {
            if self.fail_irq {
                return Err(());
            }
            let count = self.irq;
            self.irq = 0;
            Ok(count)
        }
    }

    #[test]
    fn reset_rings_and_enumerate_expected_alc256() {
        let mut controller = Controller::new(Fake::new());
        assert_eq!(controller.reset().unwrap(), 1);
        controller.start_command_rings().unwrap();
        let inventory = controller.enumerate_codec(0).unwrap();
        assert_eq!(inventory.vendor_device, EXPECTED_CODEC_VENDOR);
        assert_eq!(inventory.function_groups, vec![1]);
        assert_eq!(
            inventory.widgets.iter().map(|w| w.node).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn bounded_speaker_playback_observes_position_and_irq_then_stops() {
        let mut controller = Controller::new(Fake::new());
        controller.reset().unwrap();
        controller.start_command_rings().unwrap();
        let report = controller.play_pcm_period(0, &[0; PCM_BYTES]).unwrap();
        assert_eq!(report.route, OutputRoute::Speaker);
        assert_eq!(report.stream_index, 1);
        assert!(report.end_position > report.start_position);
        assert_eq!(report.irq_count, 1);
    }

    #[test]
    fn playback_io_error_still_mutes_disconnects_and_resets_stream() {
        let mut fake = Fake::new();
        fake.fail_irq = true;
        let mut controller = Controller::new(fake);
        controller.reset().unwrap();
        controller.start_command_rings().unwrap();

        assert!(matches!(
            controller.play_pcm_period(0, &[0; PCM_BYTES]),
            Err(Error::Io(()))
        ));
        assert_eq!(controller.io.read32(0xa0).unwrap(), 0);
        let commands = controller.io.commands.borrow();
        assert_eq!(
            &commands[commands.len() - 3..],
            &[
                (0x02 << 20) | 0x070600, // disconnect converter
                (0x14 << 20) | 0x070700, // disable speaker pin output
                (0x14 << 20) | 0x070c00, // disable speaker EAPD
            ]
        );
    }
}
