//! Images shared between APIs on Windows, by NT handle: a Direct3D 11 or
//! 12 texture, or another Vulkan device's memory, described for
//! [`Import`](crate::Import).

use std::os::windows::io::BorrowedHandle;

use crate::Format;

/// An image another API shares through a handle. The handle is borrowed
/// for the import; the GPU keeps its own reference to the memory, so the
/// handle may be closed once the import's scope is open.
#[derive(Debug, Clone, Copy)]
pub struct SharedHandle<'a> {
    /// The handle.
    pub handle: BorrowedHandle<'a>,
    /// What made it.
    pub kind: HandleKind,
    /// Its width, in pixels.
    pub width: u32,
    /// Its height, in pixels.
    pub height: u32,
    /// Its pixel format.
    pub format: Format,
}

/// What a [`SharedHandle`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HandleKind {
    /// Vulkan memory exported as an NT handle.
    Memory,
    /// A Direct3D 11 texture, shared as an NT handle.
    D3d11Texture,
    /// A Direct3D 12 resource, shared as an NT handle.
    D3d12Resource,
}
