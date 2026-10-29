//! Images shared on Apple systems, as IOSurfaces: what Core Animation,
//! AVFoundation or another process hands over, described for
//! [`Import`](crate::Import). Imported through MoltenVK.

use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr::NonNull;

use crate::Format;

/// An IOSurface, borrowed for the import: the GPU keeps its own
/// reference while the import's scope is open.
#[derive(Debug, Clone, Copy)]
pub struct IoSurface<'a> {
    surface: NonNull<c_void>,
    /// Its width, in pixels.
    pub width: u32,
    /// Its height, in pixels.
    pub height: u32,
    /// Its pixel format.
    pub format: Format,
    surface_life: PhantomData<&'a ()>,
}

impl IoSurface<'_> {
    /// Describes the IOSurface `surface` (an `IOSurfaceRef`).
    ///
    /// # Safety
    ///
    /// `surface` must be a valid `IOSurfaceRef`, of `width` by `height`
    /// pixels in `format`, alive for the borrow.
    #[allow(unsafe_code)]
    pub const unsafe fn from_raw(
        surface: NonNull<c_void>,
        width: u32,
        height: u32,
        format: Format,
    ) -> Self {
        Self {
            surface,
            width,
            height,
            format,
            surface_life: PhantomData,
        }
    }

    /// The `IOSurfaceRef`.
    pub const fn as_ptr(&self) -> NonNull<c_void> {
        self.surface
    }
}

// SAFETY: IOSurfaces are thread-safe Core Foundation objects; this only
// borrows one.
#[allow(unsafe_code)]
unsafe impl Send for IoSurface<'_> {}
// SAFETY: as above.
#[allow(unsafe_code)]
unsafe impl Sync for IoSurface<'_> {}
