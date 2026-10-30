//! Presenting: images shown in a window, through a swapchain.

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use crate::{Format, ImageUsage, Swapchain};

/// Where a swapchain's images are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// A window of the platform's: X11 (Xlib or XCB), Wayland or Win32.
    Raw {
        /// Its display (or connection).
        display: RawDisplayHandle,
        /// The window.
        window: RawWindowHandle,
    },
    /// Nowhere: presented images go unseen (`VK_EXT_headless_surface`, on
    /// a device that has it). For tests, and for rendering a window's
    /// frames without one.
    Headless,
}

/// How presented images reach the window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PresentMode {
    /// One per display refresh, in order: no tearing; [`NextFrame`] waits
    /// when every image is queued. Every device has it.
    #[default]
    Fifo,
    /// One per refresh, the newest: no tearing, and no waiting.
    Mailbox,
    /// At once: may tear.
    Immediate,
}

/// Create a swapchain on `window`, lending it for a scope: images of
/// `width` by `height` in `format` (one the window takes), each drawn into
/// and presented in turn ([`NextFrame`], [`Present`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateSwapchain {
    /// Where the images are shown.
    pub window: Window,
    /// Their width.
    pub width: u32,
    /// Their height.
    pub height: u32,
    /// Their format: [`Format::Bgra8Srgb`] unless set.
    pub format: Format,
    /// How they reach the window.
    pub mode: PresentMode,
    /// What else the images are for, beyond drawing into
    /// ([`ImageUsage::TARGET`], always).
    pub usage: ImageUsage,
}

impl CreateSwapchain {
    /// A swapchain on `window` of `display`, `width` by `height`.
    ///
    /// # Safety
    ///
    /// The window and display stay alive for the swapchain's scope.
    #[allow(unsafe_code)] // The contract above: Vulkan holds the window.
    pub const unsafe fn new(
        display: RawDisplayHandle,
        window: RawWindowHandle,
        width: u32,
        height: u32,
    ) -> Self {
        Self::on(Window::Raw { display, window }, width, height)
    }

    /// A swapchain presenting nowhere: see [`Window::Headless`].
    pub const fn headless(width: u32, height: u32) -> Self {
        Self::on(Window::Headless, width, height)
    }

    const fn on(window: Window, width: u32, height: u32) -> Self {
        Self {
            window,
            width,
            height,
            format: Format::Bgra8Srgb,
            mode: PresentMode::Fifo,
            usage: ImageUsage::TARGET,
        }
    }

    /// In `format`.
    #[must_use]
    pub const fn format(mut self, format: Format) -> Self {
        self.format = format;
        self
    }

    /// Presented by `mode`, where the window has it ([`PresentMode::Fifo`]
    /// where it does not).
    #[must_use]
    pub const fn mode(mut self, mode: PresentMode) -> Self {
        self.mode = mode;
        self
    }

    /// Its images also for `usage` (sampled, or written as storage).
    #[must_use]
    pub const fn usage(mut self, usage: ImageUsage) -> Self {
        self.usage = usage;
        self
    }
}

/// Wait for the swapchain's next image, and lend it until [`Present`]: an
/// [`Image`](crate::Image) like any other, its contents undefined, to draw
/// into. Fails with [`GpuError::Outdated`](crate::GpuError::Outdated) once
/// the window has changed size: [`ResizeSwapchain`], then try again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NextFrame(pub Swapchain);

/// Show the image [`NextFrame`] lent, once the work before this is done:
/// sends that work to the GPU, without waiting for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Present(pub Swapchain);

/// Remake a swapchain's images at a new size: after
/// [`GpuError::Outdated`](crate::GpuError::Outdated), or when the window
/// is resized. Not while an image is lent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeSwapchain {
    /// The swapchain.
    pub swapchain: Swapchain,
    /// The new width.
    pub width: u32,
    /// The new height.
    pub height: u32,
}

// SAFETY: the handles are identifiers of a window the caller keeps alive
// for the swapchain's scope (`CreateSwapchain::new`'s contract); Vulkan
// takes them from any thread.
#[allow(unsafe_code)]
unsafe impl Send for Window {}
// SAFETY: as above; nothing is read through them but by Vulkan.
#[allow(unsafe_code)]
unsafe impl Sync for Window {}
