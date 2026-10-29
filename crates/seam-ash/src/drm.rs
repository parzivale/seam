//! Buffers shared as dmabufs, on Linux and the BSDs: what a Wayland
//! client or GBM hands over, described for [`Import`](crate::Import).
//!
//! A dmabuf is GPU memory behind a file descriptor. Its contents are never
//! read through the descriptor: a handler maps the memory into the GPU
//! directly. These types only describe it; the descriptors are owned
//! here, so a [`Dmabuf`] closes them when dropped, and an import borrows
//! it.

use std::os::fd::OwnedFd;
use std::vec::Vec;

use seam_core::{Effect, Operation, Query};

use crate::{Format, GpuError, effects};

/// A dmabuf: an image in GPU memory, by its planes' descriptors, its
/// size, pixel format and memory layout.
#[derive(Debug)]
pub struct Dmabuf {
    /// Its width, in pixels.
    pub width: u32,
    /// Its height, in pixels.
    pub height: u32,
    /// Its pixel format.
    pub format: Fourcc,
    /// How its pixels are laid out in memory.
    pub modifier: Modifier,
    /// Its planes, in order: one for the RGB formats.
    pub planes: Vec<Plane>,
}

/// One plane of a [`Dmabuf`].
#[derive(Debug)]
pub struct Plane {
    /// The memory it is in.
    pub fd: OwnedFd,
    /// Where it starts in that memory, in bytes.
    pub offset: u32,
    /// The bytes from one row to the next.
    pub stride: u32,
}

/// A DRM pixel format: four characters, as in `drm_fourcc.h`. The names
/// give the channels from the most significant bit of a little-endian
/// pixel, so [`ARGB8888`](Self::ARGB8888) is the bytes B, G, R, A.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fourcc(pub u32);

impl Fourcc {
    /// 8-bit blue, green, red and alpha bytes.
    pub const ARGB8888: Self = Self::from_chars(*b"AR24");
    /// As [`ARGB8888`](Self::ARGB8888), with the alpha byte unused.
    pub const XRGB8888: Self = Self::from_chars(*b"XR24");
    /// 8-bit red, green, blue and alpha bytes.
    pub const ABGR8888: Self = Self::from_chars(*b"AB24");
    /// As [`ABGR8888`](Self::ABGR8888), with the alpha byte unused.
    pub const XBGR8888: Self = Self::from_chars(*b"XB24");
    /// 16-bit float red, green, blue and alpha.
    pub const ABGR16161616F: Self = Self::from_chars(*b"AB4H");
    /// As [`ABGR16161616F`](Self::ABGR16161616F), with alpha unused.
    pub const XBGR16161616F: Self = Self::from_chars(*b"XB4H");

    /// The formats this crate imports.
    pub const SUPPORTED: [Self; 6] = [
        Self::ARGB8888,
        Self::XRGB8888,
        Self::ABGR8888,
        Self::XBGR8888,
        Self::ABGR16161616F,
        Self::XBGR16161616F,
    ];

    /// The code for four characters.
    pub const fn from_chars(c: [u8; 4]) -> Self {
        Self(u32::from_le_bytes(c))
    }

    /// The image format it is read as, and whether its alpha is unused (so
    /// reads see it as opaque).
    pub(crate) fn format(self) -> Option<(Format, bool)> {
        Some(match self {
            Self::ARGB8888 => (Format::Bgra8Unorm, false),
            Self::XRGB8888 => (Format::Bgra8Unorm, true),
            Self::ABGR8888 => (Format::Rgba8Unorm, false),
            Self::XBGR8888 => (Format::Rgba8Unorm, true),
            Self::ABGR16161616F => (Format::Rgba16Float, false),
            Self::XBGR16161616F => (Format::Rgba16Float, true),
            _ => return None,
        })
    }
}

impl core::fmt::Debug for Fourcc {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let c = self.0.to_le_bytes();
        if c.iter().all(u8::is_ascii_graphic) {
            write!(
                f,
                "Fourcc({})",
                c.map(char::from).iter().collect::<std::string::String>()
            )
        } else {
            write!(f, "Fourcc({:#010x})", self.0)
        }
    }
}

/// A DRM format modifier: how a buffer's pixels are laid out (linear, or
/// a vendor's tiling and compression), as in `drm_fourcc.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Modifier(pub u64);

impl Modifier {
    /// Rows of pixels one after another.
    pub const LINEAR: Self = Self(0);
    /// No modifier given: the layout is whatever the driver that made the
    /// buffer chose. Not importable here.
    pub const INVALID: Self = Self((1 << 56) - 1);
}

/// A format and modifier pair a handler can import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmabufFormat {
    /// The pixel format.
    pub fourcc: Fourcc,
    /// A memory layout it imports in.
    pub modifier: Modifier,
}

/// Which dmabufs this GPU imports: every format and modifier pair, for a
/// compositor to offer clients (the linux-dmabuf protocol's feedback). A
/// query.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DmabufFormats;

impl Operation for DmabufFormats {
    type Outcome = Result<Vec<DmabufFormat>, GpuError>;
}

impl Query for DmabufFormats {}

impl Effect for DmabufFormats {
    type Key = effects::DmabufFormats;
}
