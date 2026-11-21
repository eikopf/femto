#![no_std]

//! Picco panel identification and blocking monochrome and four-level updates.
extern crate alloc;
use alloc::vec::Vec;
pub mod battery;
pub mod clock;
pub mod console;
pub mod frontlight;
pub mod idle;
pub mod input;
pub mod orientation;
mod ssd2677;
pub mod touch;
mod waveform;
pub mod wifi;

pub const WIDTH: usize = 800;
pub const HEIGHT: usize = 480;
pub const PLANE_BYTES: usize = WIDTH * HEIGHT / 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Yrd0426,
    Depg0397,
    Opm040b3,
    Se0400nqw47,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refresh {
    Full,
    Fast,
}

/// Identification follows the Picco user-ID and revision replies.
pub fn identify(id: [u8; 10], revision: [u8; 3]) -> Option<Panel> {
    const YRD: [u8; 10] = [0xca, 0xfe, 0, 0x16, 0x77, 2, 0x10, 1, 0, 0x91];
    if id == YRD || id == [0, 0xaf, 0, 1, 0xaa, 9, 0x12, 0x20, 0x69, 0] {
        Some(Panel::Yrd0426)
    } else if id[..5] == YRD[..5] {
        Some(Panel::Depg0397)
    } else if id == *b"W040B3\0\0\0\0" {
        Some(Panel::Opm040b3)
    } else if revision == [7, 1, 1] {
        Some(Panel::Se0400nqw47)
    } else {
        None
    }
}

/// Each command and its data complete before return. Idle waits must time out.
pub trait Bus {
    type Error;
    fn reset(&mut self) -> Result<(), Self::Error>;
    fn command(&mut self, command: u8, data: &[u8]) -> Result<(), Self::Error>;
    /// Streams one command's data with chip select held throughout the transfer.
    fn command_stream(
        &mut self,
        command: u8,
        data: impl Iterator<Item = u8>,
    ) -> Result<(), Self::Error>;
    fn delay_ms(&mut self, ms: u32);
    fn wait_idle(&mut self, timeout_ms: u32) -> Result<(), Self::Error>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    Bus(E),
    Allocation,
    FrameSize,
    FrameEncoding,
    NotReady,
}

pub struct Display<B> {
    bus: B,
    panel: Panel,
    ready: bool,
    previous: Vec<u8>,
    /// Reused base plane for anti-aliased updates.
    scratch: Vec<u8>,
    needs_full: bool,
}

impl<B: Bus> Display<B> {
    pub fn new(bus: B, panel: Panel) -> Result<Self, Error<B::Error>> {
        let mut previous = Vec::new();
        if panel == Panel::Se0400nqw47 {
            previous
                .try_reserve_exact(PLANE_BYTES)
                .map_err(|_| Error::Allocation)?;
            previous.resize(PLANE_BYTES, 255);
        }
        Ok(Self {
            bus,
            panel,
            ready: false,
            previous,
            scratch: Vec::new(),
            needs_full: true,
        })
    }

    fn command(&mut self, command: u8, data: &[u8]) -> Result<(), Error<B::Error>> {
        self.bus.command(command, data).map_err(Error::Bus)
    }

    fn idle(&mut self) -> Result<(), Error<B::Error>> {
        self.bus.wait_idle(10_000).map_err(Error::Bus)
    }

    fn area(&mut self) -> Result<(), Error<B::Error>> {
        self.command(0x11, &[1])?;
        self.command(0x44, &[0, 0, 0x1f, 3])?;
        self.command(0x45, &[0xdf, 1, 0, 0])?;
        self.command(0x4e, &[0, 0])?;
        self.command(0x4f, &[0xdf, 1])
    }

    pub fn initialize(&mut self) -> Result<(), Error<B::Error>> {
        self.ready = false;
        self.needs_full = true;
        self.bus.reset().map_err(Error::Bus)?;
        if self.panel == Panel::Se0400nqw47 {
            return self.initialize_ssd2677();
        }
        self.command(0x12, &[])?;
        self.bus.delay_ms(10);
        self.idle()?;
        self.command(0x18, &[0x80])?;
        self.command(0x0c, &[0xae, 0xc7, 0xc3, 0xc0, 0x80])?;
        self.command(0x01, &[0xdf, 1, 2])?;
        self.command(0x3c, &[if self.panel == Panel::Opm040b3 { 3 } else { 1 }])?;
        self.area()?;
        for command in [0x46, 0x47] {
            self.command(command, &[0xf7])?;
            self.bus.delay_ms(10);
            self.idle()?;
        }
        self.ready = true;
        Ok(())
    }

    /// Power down the controller and invalidate all differential-refresh state.
    /// Success or bus failure requires initialize() before another refresh.
    pub fn sleep(&mut self) -> Result<(), Error<B::Error>> {
        if !self.ready {
            return Err(Error::NotReady);
        }
        self.ready = false;
        self.needs_full = true;
        self.idle()?;
        if self.panel == Panel::Se0400nqw47 {
            // Every supported SSD2677 refresh completes with power-off.
            self.command(0x07, &[0xa5])?;
        } else {
            self.command(0x3c, &[if self.panel == Panel::Opm040b3 { 3 } else { 1 }])?;
            self.command(0x22, &[0x03])?;
            self.command(0x20, &[])?;
            self.bus.delay_ms(10);
            self.idle()?;
            self.command(0x10, &[0x03])?;
        }
        Ok(())
    }

    /// One bit per native pixel, most significant bit first; one means white.
    /// Any transport failure requires reinitialization before another update.
    pub fn full_refresh(&mut self, plane: &[u8]) -> Result<(), Error<B::Error>> {
        self.refresh(plane, Refresh::Full).map(|_| ())
    }

    /// Returns the executed mode. The first update after initialization is full.
    pub fn refresh(
        &mut self,
        plane: &[u8],
        requested: Refresh,
    ) -> Result<Refresh, Error<B::Error>> {
        if plane.len() != PLANE_BYTES {
            return Err(Error::FrameSize);
        }
        if !self.ready {
            return Err(Error::NotReady);
        }
        let mode = if self.needs_full {
            Refresh::Full
        } else {
            requested
        };
        self.needs_full = true;
        self.refresh_inner(plane, mode)?;
        self.needs_full = false;
        Ok(mode)
    }

    fn refresh_inner(&mut self, plane: &[u8], mode: Refresh) -> Result<(), Error<B::Error>> {
        self.ready = false;
        if self.panel == Panel::Se0400nqw47 {
            return self.refresh_ssd2677(plane, mode);
        }
        self.area()?;
        self.command(0x24, plane)?;
        if mode == Refresh::Full {
            self.command(0x26, plane)?;
        }
        self.command(0x21, &[if mode == Refresh::Fast { 0 } else { 0x40 }])?;
        let sequence = if mode == Refresh::Fast {
            self.load_lut(if self.panel == Panel::Depg0397 {
                &waveform::DEPG_FAST
            } else {
                &waveform::YRD_OPM_FAST
            })?;
            0xcf
        } else if self.panel == Panel::Depg0397 {
            self.load_lut(&waveform::DEPG_FULL)?;
            0xe7
        } else {
            0xf7
        };
        self.command(0x22, &[sequence])?;
        self.command(0x20, &[])?;
        self.bus.delay_ms(10);
        self.idle()?;
        self.area()?;
        self.command(0x24, plane)?;
        self.command(0x26, plane)?;
        self.ready = true;
        Ok(())
    }

    fn load_lut(&mut self, lut: &[u8; 110]) -> Result<(), Error<B::Error>> {
        self.command(0x32, &lut[..105])?;
        self.command(0x03, &lut[105..106])?;
        self.command(0x04, &lut[106..109])?;
        self.command(0x2c, &lut[109..110])?;
        self.command(0x3c, &[0x80])
    }

    /// Four-level update without a full-panel flash: a differential
    /// black-and-white refresh followed by a pass that moves only gray pixels.
    /// Planes use the absolute encoding of [`Self::grayscale`]. Returns the
    /// executed base mode. Panels without an overlay waveform use the absolute
    /// update and report a full refresh.
    pub fn antialiased(
        &mut self,
        msb: &[u8],
        lsb: &[u8],
        requested: Refresh,
    ) -> Result<Refresh, Error<B::Error>> {
        if self.panel != Panel::Se0400nqw47 {
            return self.grayscale(msb, lsb).map(|()| Refresh::Full);
        }
        if msb.len() != PLANE_BYTES || lsb.len() != PLANE_BYTES {
            return Err(Error::FrameSize);
        }
        if !self.ready {
            return Err(Error::NotReady);
        }
        let mode = if self.needs_full {
            Refresh::Full
        } else {
            requested
        };
        self.ready = false;
        self.needs_full = true;
        self.antialiased_ssd2677(msb, lsb, mode)?;
        self.needs_full = false;
        Ok(mode)
    }

    /// Absolute four-level update: black=00, dark=01, light=10, white=11.
    /// Both arguments are native MSB-first bitplanes. The next monochrome
    /// update is forced full because grayscale is not a differential baseline.
    pub fn grayscale(&mut self, msb: &[u8], lsb: &[u8]) -> Result<(), Error<B::Error>> {
        if msb.len() != PLANE_BYTES || lsb.len() != PLANE_BYTES {
            return Err(Error::FrameSize);
        }
        if !self.ready {
            return Err(Error::NotReady);
        }
        self.ready = false;
        self.needs_full = true;
        if self.panel == Panel::Se0400nqw47 {
            return self.grayscale_ssd2677(msb, lsb);
        }
        self.area()?;
        self.bus
            .command_stream(0x24, lsb.iter().map(|byte| !byte))
            .map_err(Error::Bus)?;
        self.area()?;
        self.bus
            .command_stream(0x26, msb.iter().map(|byte| !byte))
            .map_err(Error::Bus)?;
        self.load_lut(&waveform::SSD1677_GRAY)?;
        self.command(0x21, &[0])?;
        self.command(0x22, &[0xcc])?;
        self.command(0x20, &[])?;
        self.bus.delay_ms(10);
        self.idle()?;
        self.command(0x3c, &[if self.panel == Panel::Opm040b3 { 3 } else { 1 }])?;
        self.command(0x22, &[3])?;
        self.command(0x20, &[])?;
        self.bus.delay_ms(10);
        self.idle()?;
        self.ready = true;
        Ok(())
    }
}

/// Rotates four-level portrait pixels into separate high and low native planes.
pub fn portrait_grayscale(
    pixels: &[u8],
    msb: &mut [u8],
    lsb: &mut [u8],
) -> Result<(), Error<core::convert::Infallible>> {
    orientation::Orientation::Portrait.grayscale(pixels, msb, lsb)
}

/// Maps 480x800 portrait pixels to native panel memory without resampling.
/// Input must already contain black/white values; grayscale is rejected.
pub fn portrait_plane(
    pixels: &[u8],
    plane: &mut [u8],
) -> Result<(), Error<core::convert::Infallible>> {
    orientation::Orientation::Portrait.monochrome(pixels, plane)
}
