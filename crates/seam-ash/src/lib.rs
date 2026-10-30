//! Vulkan for seam: the GPU as effects.
//!
//! A core sees the GPU as a few kinds of thing, and nothing else:
//!
//! - **Resources**, as scopes: [`CreateBuffer`], [`CreateImage`],
//!   [`CreateSampler`], [`CreateProgram`] and [`CreateQueries`] lend a
//!   [`Buffer`], an [`Image`], a [`Sampler`], a [`Program`] or
//!   [`Queries`] (plain IDs, the same for every handler) and destroy it
//!   when the scope closes.
//! - **Moving data**: [`Upload`], [`UploadImage`] and [`FillBuffer`] put
//!   bytes in, [`Download`] and [`DownloadImage`] take them out (images by
//!   [`ImageRegion`]), [`CopyBuffer`], [`CopyImage`] and [`BlitImage`]
//!   move them on the GPU, and [`GenerateMips`] fills a mip chain.
//! - **Running programs**: [`Dispatch`] runs a compute program, [`Draw`]
//!   draws into an image, each with the resources it reads and writes
//!   bound in order (a binding may be an array:
//!   [`CreateProgram::counts`]), and specialization constants
//!   ([`CreateProgram::constants`]). [`DispatchIndirect`] and
//!   [`Draw::indirect`] take their counts from a buffer; a shader reaches
//!   a buffer through its [`BufferAddress`] too.
//! - **Measuring**: [`WriteTimestamp`] and [`Draw::occlusion`] record into
//!   [`Queries`], which [`ReadQueries`] reads; [`DeviceInfo`] reports the
//!   device's [`GpuFeatures`] and [`Limits`], its subgroups included.
//! - **Waiting**: [`Download`], [`DownloadImage`], [`ReadQueries`] and
//!   [`Finish`] wait for the work before them; nothing else does.
//! - **Submitting without waiting**: [`Submit`] is a scope that sends the
//!   work so far to the GPU and lends a submission: [`IsDone`] says
//!   whether it has finished, and closing the scope waits for it. So the
//!   CPU does other work while the GPU runs.
//! - **Presenting**: [`CreateSwapchain`] lends a [`Swapchain`] on a
//!   window; [`NextFrame`] lends its next image, to draw into, and
//!   [`Present`] shows it (see [Presenting](#presenting)).
//! - **Images from elsewhere**: [`Import`] lends a Wayland client's
//!   dmabuf ([`drm`], Linux and the BSDs), a shared Direct3D texture
//!   ([`win32`], Windows) or an IOSurface ([`metal`], Apple systems) as an
//!   [`Image`], without a copy on the CPU.
//!
//! Synchronization is the handler's business: it tracks what each step
//! reads and writes, and inserts the barriers and image layout changes
//! Vulkan needs. Work is recorded as it is performed and submitted when
//! something waits.
//!
//! Two handlers:
//!
//! - [`Ash`] (feature `ash`, on by default): real Vulkan 1.3, through ash.
//!   It implements the operations twice from one implementation: the
//!   blocking form (`Perform`) waits on the calling thread, and the async
//!   form (`AsyncPerform`, `SendPerform`) awaits a waiter thread, under any
//!   executor. Everything but the waiting is the same sync code. Waits
//!   spin briefly before sleeping or handing off; [`AshOptions`] sets for
//!   how long, and the waiter thread's stack size.
//! - [`SimGpu`]: a simulated GPU that keeps buffers and images in memory,
//!   moves real bytes, checks every operation as `Ash` does, and runs a
//!   program by a CPU function a test gives it, by the program's label.
//!
//! ```
//! use seam_core::prelude::*;
//! use seam_ash::*;
//!
//! /// Doubles each number on the GPU.
//! fn double<G>(gpu: &mut G, shader: &[u32], numbers: &[u32]) -> Result<Vec<u32>, GpuError>
//! where
//!     G: Provide<CreateBuffer> + for<'a> Provide<CreateProgram<'a>>
//!         + for<'a> Perform<Upload<'a>> + for<'a> Perform<Dispatch<'a>>
//!         + for<'a> Perform<Download<'a>>,
//! {
//!     let bytes: Vec<u8> = numbers.iter().flat_map(|n| n.to_le_bytes()).collect();
//!     let program = CreateProgram::compute("double", shader, "main", &[BindingKind::StorageBuffer]);
//!     gpu.acquire(program, |gpu, program| {
//!         let size = bytes.len() as u64;
//!         gpu.acquire(CreateBuffer::new(size, BufferUsage::STORAGE), |gpu, buf| {
//!             gpu.perform(Upload::new(*buf, &bytes))?;
//!             gpu.perform(Dispatch::new(*program, &[Bind::Storage(*buf)], [numbers.len() as u32, 1, 1]))?;
//!             let mut out = vec![0; bytes.len()];
//!             gpu.perform(Download::new(*buf, &mut out))?;
//!             Ok(out.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect())
//!         })
//!     })
//! }
//!
//! // In a test: the program's work, done on the CPU.
//! let mut gpu = SimGpu::new();
//! gpu.emulate("double", |run| {
//!     for chunk in run.buffer(0).chunks_mut(4) {
//!         let n = u32::from_le_bytes(chunk.try_into().unwrap());
//!         chunk.copy_from_slice(&(n * 2).to_le_bytes());
//!     }
//! });
//! assert_eq!(double(&mut gpu, &[], &[1, 2, 21]).unwrap(), [2, 4, 42]);
//! ```
//! # Drawing
//!
//! A draw program's [`DrawPipeline`] says what it draws and how: up to
//! [`MAX_TARGETS`] colour targets ([`ColorTarget`]: a format, a [`Blend`]
//! and a [`ColorMask`] each), a depth and stencil test ([`DepthState`]),
//! up to [`MAX_VERTEX_BUFFERS`] vertex buffers, per vertex or per instance
//! ([`VertexLayout`]), its [`Raster`] state (topology, culling, front face,
//! polygon mode, primitive restart) and its samples per pixel. A [`Draw`]
//! then gives the images: an [`Attachment`] per target (a mip level and
//! layer, what to [`Load`] first, and a single-sampled image to resolve a
//! multisampled one into), a [`DepthAttachment`], the vertex buffers, an
//! [`IndexBuffer`], instances, a [`Viewport`], a scissor [`Rect`] and a
//! stencil reference. Everything is checked against the program, the same
//! way by every handler, before anything reaches the GPU.
//!
//! Images ([`CreateImage`]) are flat, cubes or volumes, with layers, mip
//! levels and samples, in colour, integer, packed, depth and stencil, and
//! BC-compressed [`Format`]s.
//!
//! # Presenting
//!
//! A swapchain is a scope on a window: an X11, Wayland or Win32 one from
//! its [raw-window-handle](https://docs.rs/raw-window-handle) handles
//! ([`CreateSwapchain::new`], unsafe as the window must outlive it), or
//! none at all ([`CreateSwapchain::headless`]). Each frame, [`NextFrame`]
//! lends an [`Image`] like any other, to draw into, copy into or read;
//! [`Present`] sends the frame's work and shows it. When the window
//! changes size, [`NextFrame`] or [`Present`] fails with
//! [`GpuError::Outdated`], and [`ResizeSwapchain`] remakes its images.
//! [`SimGpu`] keeps what each swapchain's window was shown
//! ([`SimGpu::frames`]), and resizes it as a user would
//! ([`SimGpu::resize_window`]).
//!
//! ```
//! use seam_ash::*;
//! use seam_core::prelude::*;
//!
//! /// Shows `frames` frames, each cleared to a brighter grey.
//! fn show<G>(gpu: &mut G, program: Program, frames: u32) -> Result<(), GpuError>
//! where
//!     G: Provide<CreateSwapchain>
//!         + Perform<NextFrame>
//!         + Perform<Present>
//!         + Perform<ResizeSwapchain>
//!         + for<'a> Perform<Draw<'a>>,
//! {
//!     let chain = CreateSwapchain::headless(64, 64).format(Format::Bgra8Unorm);
//!     gpu.acquire(chain, |gpu, chain| {
//!         for i in 0..frames {
//!             let image = match gpu.perform(NextFrame(*chain)) {
//!                 Err(GpuError::Outdated) => {
//!                     let size = ResizeSwapchain { swapchain: *chain, width: 32, height: 32 };
//!                     gpu.perform(size)?;
//!                     gpu.perform(NextFrame(*chain))?
//!                 }
//!                 image => image?,
//!             };
//!             let grey = i as f32 / frames as f32;
//!             gpu.perform(Draw::new(program, image, 0).clear([grey, grey, grey, 1.0]))?;
//!             gpu.perform(Present(*chain))?;
//!         }
//!         Ok(())
//!     })
//! }
//! # let mut gpu = SimGpu::new();
//! # let spirv = [0x0723_0203];
//! # let pipeline = DrawPipeline::new(&spirv, "vs", &spirv, "fs", Format::Bgra8Unorm);
//! # let program = CreateProgram::draw("clear", ProgramKind::Draw(pipeline), &[]);
//! # gpu.clone().acquire(program, |_, p| show(&mut gpu, *p, 3)).unwrap();
//! ```
//!
//! # Shaders
//!
//! A program is SPIR-V words, from whatever compiles to it: WGSL through
//! naga, GLSL through glslang, HLSL through DXC, or Rust through
//! [rust-gpu](https://github.com/Rust-GPU/rust-gpu). Bindings are set 0,
//! binding `i` for the program's `i`th [`BindingKind`]; a combined image
//! sampler (GLSL's `sampler2D`, rust-gpu's `SampledImage`) is
//! [`BindingKind::SampledImage`], and a texture and a sampler apart (WGSL,
//! HLSL) are [`BindingKind::Texture`] and [`BindingKind::Sampler`].
//!
//! Shaders in Rust build for the CPU as well as for SPIR-V, so a
//! [`SimGpu`] emulator can call the shader itself: the test runs the code
//! the GPU runs. This repository's `crates/seam-ash/shaders` does so,
//! and its tests run both halves:
//!
//! ```ignore
//! // The shader crate, built by rust-gpu to SPIR-V, and for the CPU.
//! #[spirv(compute(threads(64)))]
//! pub fn double(
//!     #[spirv(global_invocation_id)] id: UVec3,
//!     #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] numbers: &mut [u32],
//! ) {
//!     if let Some(n) = numbers.get_mut(id.x as usize) {
//!         *n *= 2;
//!     }
//! }
//!
//! // On the GPU: its SPIR-V.
//! let spirv = bytes_u32(include_bytes!("shaders.spv"));
//! CreateProgram::compute("double", &spirv, "double", &[BindingKind::StorageBuffer]);
//!
//! // In the simulation: the same function, invocation by invocation.
//! sim.emulate("double", |run| {
//!     let mut numbers = bytes_u32(run.buffer(0));
//!     for x in 0..run.groups()[0] * 64 {
//!         shaders::double(UVec3::new(x, 0, 0), &mut numbers);
//!     }
//!     run.buffer(0).copy_from_slice(&u32_bytes(&numbers));
//! });
//! ```
//!
//! rust-gpu's image types have no CPU form, so a shader that samples
//! keeps its maths in plain functions both sides call, and its emulator
//! samples with [`Run::sample`]. `Ash` turns on the Vulkan memory model
//! where the device has it, which rust-gpu's shaders declare.
//!
//! # In a program
//!
//! `Ash` is a part like any other: tuned with [`AshOptions`], it serves the
//! GPU keys of [`effects`] it is listed for, and every clone is the same
//! device, so tasks share it. The async form waits without blocking, under
//! any executor:
//!
//! ```no_run
//! # #[cfg(feature = "ash")] {
//! use core::time::Duration;
//! use seam_ash::{effects as gpu, *};
//! use seam_core::prelude::*;
//!
//! let options = AshOptions {
//!     spin: Duration::from_micros(100),
//!     ..AshOptions::default()
//! };
//! let mut root = seam_core::root![
//!     Ash<gpu::CreateBuffer, gpu::Upload, gpu::Download>::with(&options)?,
//! ];
//! let out = seam_smol::block_on(root.acquire_send(
//!     CreateBuffer::new(4, BufferUsage::STORAGE),
//!     async |root, buf| {
//!         root.perform_send(Upload::new(*buf, &[1, 2, 3, 4])).await?;
//!         let mut out = [0; 4];
//!         root.perform_send(Download::new(*buf, &mut out)).await?;
//!         Ok::<_, GpuError>(out)
//!     },
//! ))?;
//! assert_eq!(out, [1, 2, 3, 4]);
//! # }
//! # Ok::<(), seam_ash::GpuError>(())
//! ```
//!
//! Through the `seam` crate, this crate is `seam::ash` (feature `ash`).
//!
//! # Features
//!
//! | Feature | Default | Gives |
//! |---|---|---|
//! | `ash` | yes | [`Ash`]: real Vulkan, through ash (the loader is opened at run time; building needs no SDK). Without it: the operations, [`SimGpu`] and the import types, with no `unsafe` |
//!
#![cfg_attr(
    feature = "ash",
    doc = "[`Ash`]: crate::Ash\n[`AshOptions`]: crate::AshOptions"
)]
#![cfg_attr(
    not(feature = "ash"),
    doc = "[`Ash`]: https://docs.rs/seam-rs-ash/latest/seam_ash/struct.Ash.html\n\
           [`AshOptions`]: https://docs.rs/seam-rs-ash/latest/seam_ash/struct.AshOptions.html"
)]
#![cfg_attr(
    not(drm),
    doc = "[`drm`]: https://docs.rs/seam-rs-ash/latest/seam_ash/drm/index.html"
)]
#![cfg_attr(
    not(windows),
    doc = "[`win32`]: https://docs.rs/seam-rs-ash/latest/x86_64-pc-windows-msvc/seam_ash/win32/index.html"
)]
#![cfg_attr(
    not(target_vendor = "apple"),
    doc = "[`metal`]: https://docs.rs/seam-rs-ash/latest/x86_64-apple-darwin/seam_ash/metal/index.html"
)]
#![cfg_attr(not(feature = "ash"), deny(unsafe_code))]

use std::string::String;
use std::vec::Vec;

use seam_core::{Acquisition, Effect, Operation, Query};

#[cfg(feature = "ash")]
#[allow(unsafe_code)] // Vulkan: every call is unsafe; see the module's docs.
mod device;
mod present;
mod render;
mod sim;
mod validate;

#[cfg(drm)]
pub mod drm;
#[cfg(target_vendor = "apple")]
pub mod metal;
#[cfg(windows)]
pub mod win32;

#[cfg(feature = "ash")]
pub use device::{Ash, AshOptions, Submission};

pub use present::*;
pub use render::*;
pub use sim::{Recorded, Run, SimGpu, SimSubmission};

macro_rules! ids {
    ($($(#[$m:meta])* $name:ident;)*) => {$(
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(u64);

        impl $name {
            /// The ID, for a handler of these operations to map to its
            /// own object.
            pub const fn raw(self) -> u64 {
                self.0
            }

            /// An ID from [`raw`](Self::raw): for a handler to hand out.
            pub const fn from_raw(raw: u64) -> Self {
                Self(raw)
            }
        }
    )*};
}

ids! {
    /// A buffer on the GPU, lent by [`CreateBuffer`].
    Buffer;
    /// An image on the GPU, lent by [`CreateImage`].
    Image;
    /// A compiled program, lent by [`CreateProgram`].
    Program;
    /// How a program samples an image, lent by [`CreateSampler`].
    Sampler;
    /// Slots for timestamps or occlusion counts, lent by
    /// [`CreateQueries`].
    Queries;
    /// Images shown in a window in turn, lent by [`CreateSwapchain`].
    Swapchain;
}

macro_rules! flags {
    ($($(#[$m:meta])* $name:ident { $($(#[$fm:meta])* $flag:ident = $bit:expr;)* })*) => {$(
        $(#[$m])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
        pub struct $name(u32);

        impl $name {
            $($(#[$fm])* pub const $flag: Self = Self(1 << $bit);)*

            /// Every flag.
            pub const ALL: Self = Self(0 $(| 1 << $bit)*);

            /// Whether every flag in `other` is set.
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }
        }

        impl core::ops::BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self {
                Self(self.0 | rhs.0)
            }
        }
    )*};
}

flags! {
    /// What a buffer is for, beyond moving data in and out (always allowed).
    BufferUsage {
        /// Vertices, for [`Draw`].
        VERTEX = 0;
        /// Indices, for [`Draw`].
        INDEX = 3;
        /// A uniform buffer, bound with [`Bind::Uniform`].
        UNIFORM = 1;
        /// A storage buffer, bound with [`Bind::Storage`].
        STORAGE = 2;
        /// The counts of an indirect draw or dispatch
        /// ([`Draw::indirect`], [`DispatchIndirect`]).
        INDIRECT = 4;
        /// Read and written through its address in a shader
        /// ([`BufferAddress`]), on a device with
        /// [`GpuFeatures::ADDRESSES`].
        ADDRESS = 5;
    }

    /// What an image is for, beyond moving data in and out (always allowed).
    ImageUsage {
        /// Sampled by a program, bound with [`Bind::Sampled`].
        SAMPLED = 0;
        /// Drawn into, as a target of [`Draw`] (its depth, for a depth
        /// format).
        TARGET = 1;
        /// Read and written by a program, bound with [`Bind::StorageImage`].
        STORAGE = 2;
    }

    /// The optional features a device has. What needs one the device
    /// lacks fails with [`GpuError::Unsupported`]; [`SimGpu`] has them
    /// all.
    GpuFeatures {
        /// Wireframe and point polygon modes ([`PolygonMode`]).
        WIREFRAME = 0;
        /// Targets blended differently from one another.
        INDEPENDENT_BLEND = 1;
        /// BC-compressed formats.
        BC = 2;
        /// Arrays of cube maps.
        CUBE_ARRAY = 3;
        /// More than one draw from one indirect buffer
        /// ([`Draw::indirect_many`]).
        MULTI_DRAW_INDIRECT = 4;
        /// Timestamps ([`QueryKind::Timestamp`]).
        TIMESTAMPS = 5;
        /// Buffer addresses ([`BufferUsage::ADDRESS`]).
        ADDRESSES = 6;
        /// Occlusion queries count samples, not only whether any passed.
        PRECISE_OCCLUSION = 7;
    }

    /// What a device's subgroups (waves, warps) can do across their
    /// invocations in compute shaders: WGSL's and SPIR-V's subgroup
    /// operations.
    SubgroupOps {
        /// Elect, barriers.
        BASIC = 0;
        /// All, any, equal.
        VOTE = 1;
        /// Sums, products, minimums, maximums, and their scans.
        ARITHMETIC = 2;
        /// Ballots and broadcasts.
        BALLOT = 3;
        /// Shuffles by index.
        SHUFFLE = 4;
        /// Shuffles up and down.
        SHUFFLE_RELATIVE = 5;
        /// Arithmetic over clusters.
        CLUSTERED = 6;
        /// Quad operations.
        QUAD = 7;
    }
}

/// An image's pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// One 8-bit channel, in [0, 1].
    R8Unorm,
    /// Two 8-bit channels, in [0, 1].
    Rg8Unorm,
    /// 8-bit red, green, blue, alpha, in [0, 1].
    Rgba8Unorm,
    /// 8-bit red, green, blue, alpha, in [-1, 1].
    Rgba8Snorm,
    /// 8-bit blue, green, red, alpha, in [0, 1]: what displays scan out.
    Bgra8Unorm,
    /// [`Rgba8Unorm`](Self::Rgba8Unorm), stored as sRGB.
    Rgba8Srgb,
    /// [`Bgra8Unorm`](Self::Bgra8Unorm), stored as sRGB.
    Bgra8Srgb,
    /// One 8-bit unsigned integer.
    R8Uint,
    /// Four 8-bit unsigned integers.
    Rgba8Uint,
    /// One 16-bit channel, in [0, 1].
    R16Unorm,
    /// 16-bit red, green, blue, alpha, in [0, 1].
    Rgba16Unorm,
    /// One 16-bit float.
    R16Float,
    /// Two 16-bit floats.
    Rg16Float,
    /// 16-bit float red, green, blue, alpha.
    Rgba16Float,
    /// One 16-bit unsigned integer.
    R16Uint,
    /// One 32-bit float.
    R32Float,
    /// Two 32-bit floats.
    Rg32Float,
    /// 32-bit float red, green, blue, alpha.
    Rgba32Float,
    /// One 32-bit unsigned integer: IDs, counts.
    R32Uint,
    /// Two 32-bit unsigned integers.
    Rg32Uint,
    /// Four 32-bit unsigned integers.
    Rgba32Uint,
    /// One 32-bit signed integer.
    R32Sint,
    /// 10-bit red, green, blue and 2-bit alpha, in [0, 1], packed in 32
    /// bits.
    Rgb10A2Unorm,
    /// Unsigned 11-bit red and green and 10-bit blue floats, packed in 32
    /// bits: HDR colour without alpha.
    Rg11B10Float,
    /// 16-bit depth.
    D16Unorm,
    /// 32-bit float depth.
    D32Float,
    /// 24-bit depth and 8-bit stencil (not on every device: see
    /// [`D32FloatS8Uint`](Self::D32FloatS8Uint)).
    D24UnormS8Uint,
    /// 32-bit float depth and 8-bit stencil.
    D32FloatS8Uint,
    /// BC1: RGB (and 1-bit alpha) in 8-byte 4x4 blocks.
    Bc1Unorm,
    /// BC1, stored as sRGB.
    Bc1Srgb,
    /// BC3: RGBA in 16-byte 4x4 blocks.
    Bc3Unorm,
    /// BC3, stored as sRGB.
    Bc3Srgb,
    /// BC4: one channel in 8-byte 4x4 blocks.
    Bc4Unorm,
    /// BC5: two channels in 16-byte 4x4 blocks: normal maps.
    Bc5Unorm,
    /// BC6H: unsigned HDR RGB in 16-byte 4x4 blocks.
    Bc6hUfloat,
    /// BC7: RGBA in 16-byte 4x4 blocks, the best quality.
    Bc7Unorm,
    /// BC7, stored as sRGB.
    Bc7Srgb,
}

impl Format {
    /// Its block: width and height in pixels, and size in bytes. A pixel
    /// (1 by 1) for every format but the block-compressed ones (4 by 4).
    pub const fn block(self) -> (u32, u32, u32) {
        match self {
            Self::Bc1Unorm | Self::Bc1Srgb | Self::Bc4Unorm => (4, 4, 8),
            Self::Bc3Unorm
            | Self::Bc3Srgb
            | Self::Bc5Unorm
            | Self::Bc6hUfloat
            | Self::Bc7Unorm
            | Self::Bc7Srgb => (4, 4, 16),
            _ => (1, 1, self.bytes_per_pixel()),
        }
    }

    /// Bytes per pixel, for every format but the block-compressed ones
    /// (for which it is 0: see [`block`](Self::block)). A depth and
    /// stencil format's is its size in memory, which uploads and downloads
    /// do not use: they move depth alone, of `D16Unorm` and `D32Float`.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::R8Unorm | Self::R8Uint => 1,
            Self::Rg8Unorm | Self::R16Unorm | Self::R16Float | Self::R16Uint | Self::D16Unorm => 2,
            Self::Rgba8Unorm
            | Self::Rgba8Snorm
            | Self::Bgra8Unorm
            | Self::Rgba8Srgb
            | Self::Bgra8Srgb
            | Self::Rgba8Uint
            | Self::Rg16Float
            | Self::R32Float
            | Self::R32Uint
            | Self::R32Sint
            | Self::Rgb10A2Unorm
            | Self::Rg11B10Float
            | Self::D32Float
            | Self::D24UnormS8Uint => 4,
            Self::D32FloatS8Uint => 5,
            Self::Rgba16Unorm | Self::Rgba16Float | Self::Rg32Float | Self::Rg32Uint => 8,
            Self::Rgba32Float | Self::Rgba32Uint => 16,
            Self::Bc1Unorm
            | Self::Bc1Srgb
            | Self::Bc3Unorm
            | Self::Bc3Srgb
            | Self::Bc4Unorm
            | Self::Bc5Unorm
            | Self::Bc6hUfloat
            | Self::Bc7Unorm
            | Self::Bc7Srgb => 0,
        }
    }

    /// The bytes of a `width` by `height` area, tightly packed (whole
    /// blocks, for a compressed format).
    pub const fn size(self, width: u32, height: u32) -> u64 {
        let (bw, bh, bytes) = self.block();
        (width.div_ceil(bw) as u64) * (height.div_ceil(bh) as u64) * bytes as u64
    }

    /// Whether it holds depth.
    pub const fn is_depth(self) -> bool {
        matches!(
            self,
            Self::D16Unorm | Self::D32Float | Self::D24UnormS8Uint | Self::D32FloatS8Uint
        )
    }

    /// Whether it holds stencil.
    pub const fn has_stencil(self) -> bool {
        matches!(self, Self::D24UnormS8Uint | Self::D32FloatS8Uint)
    }

    /// Whether it is block-compressed: sampled and uploaded, not drawn
    /// into.
    pub const fn is_compressed(self) -> bool {
        self.block().0 > 1
    }

    /// Whether it holds integers, read as integers by shaders (and not
    /// blended or filtered).
    pub const fn is_integer(self) -> bool {
        matches!(
            self,
            Self::R8Uint
                | Self::Rgba8Uint
                | Self::R16Uint
                | Self::R32Uint
                | Self::Rg32Uint
                | Self::Rgba32Uint
                | Self::R32Sint
        )
    }
}

/// How a [`Sampler`] reads between texels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Filter {
    /// The nearest texel.
    Nearest,
    /// A blend of the four nearest texels, by distance.
    Linear,
}

/// What a [`Sampler`] reads outside the image, per axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressMode {
    /// The edge texel.
    ClampToEdge,
    /// The image again, tiled.
    Repeat,
    /// The image again, mirrored every other time.
    MirroredRepeat,
    /// The sampler's [`BorderColor`].
    ClampToBorder,
}

/// The colour a [`Sampler`] with [`AddressMode::ClampToBorder`] reads
/// outside the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BorderColor {
    /// `[0, 0, 0, 0]`.
    TransparentBlack,
    /// `[0, 0, 0, 1]`.
    OpaqueBlack,
    /// `[1, 1, 1, 1]`.
    OpaqueWhite,
}

impl BorderColor {
    /// The colour, as RGBA.
    pub const fn rgba(self) -> [f32; 4] {
        match self {
            Self::TransparentBlack => [0.0, 0.0, 0.0, 0.0],
            Self::OpaqueBlack => [0.0, 0.0, 0.0, 1.0],
            Self::OpaqueWhite => [1.0; 4],
        }
    }
}

/// Create a sampler, lending it for a scope: how a program reads an image
/// bound with [`Bind::Sampled`]. Any stage samples, the vertex stage too
/// (there with `textureLod` or `texelFetch`, as a vertex shader has no
/// derivatives for `texture`).
///
/// Images have one level, so sampling is at level 0 and the
/// magnification filter applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CreateSampler {
    /// The filter when a texel covers more than a pixel.
    pub mag: Filter,
    /// The filter when it covers less.
    pub min: Filter,
    /// Outside the image horizontally.
    pub address_u: AddressMode,
    /// Outside the image vertically.
    pub address_v: AddressMode,
    /// The colour outside, for [`AddressMode::ClampToBorder`].
    pub border: BorderColor,
}

impl CreateSampler {
    /// Describes a sampler filtering with `filter` both ways, clamped to
    /// the edge.
    pub const fn new(filter: Filter) -> Self {
        Self {
            mag: filter,
            min: filter,
            address_u: AddressMode::ClampToEdge,
            address_v: AddressMode::ClampToEdge,
            border: BorderColor::TransparentBlack,
        }
    }

    /// Filters with `mag` when magnifying and `min` when minifying.
    #[must_use]
    pub const fn filters(mut self, mag: Filter, min: Filter) -> Self {
        self.mag = mag;
        self.min = min;
        self
    }

    /// Reads outside the image by `mode`, on both axes.
    #[must_use]
    pub const fn address(self, mode: AddressMode) -> Self {
        self.address_uv(mode, mode)
    }

    /// Reads outside the image by `u` horizontally and `v` vertically.
    #[must_use]
    pub const fn address_uv(mut self, u: AddressMode, v: AddressMode) -> Self {
        self.address_u = u;
        self.address_v = v;
        self
    }

    /// Reads `color` outside the image, with [`AddressMode::ClampToBorder`].
    #[must_use]
    pub const fn border(mut self, color: BorderColor) -> Self {
        self.border = color;
        self
    }
}

/// What a program's binding is, at set 0, binding `i` for the `i`th kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingKind {
    /// A uniform buffer.
    UniformBuffer,
    /// A storage buffer.
    StorageBuffer,
    /// A sampled image: a combined image sampler (GLSL's `sampler2D`,
    /// rust-gpu's `SampledImage`), bound with an image and the [`Sampler`]
    /// that reads it.
    SampledImage,
    /// An image to sample, without its sampler (WGSL's `texture_2d`,
    /// HLSL's `Texture2D`): paired in the shader with a
    /// [`Sampler`](BindingKind::Sampler) binding.
    Texture,
    /// A sampler alone (WGSL's and HLSL's `sampler`).
    Sampler,
    /// A storage image.
    StorageImage,
}

/// A resource bound to a program, in binding order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bind {
    /// A buffer read as uniforms.
    Uniform(Buffer),
    /// A buffer the program may read and write.
    Storage(Buffer),
    /// An image the program samples, with the sampler that reads it.
    Sampled(Image, Sampler),
    /// An image the program samples, with a sampler bound on its own.
    Texture(Image),
    /// A sampler, for the program's textures.
    Sampler(Sampler),
    /// An image the program may read and write.
    StorageImage(Image),
}

impl Bind {
    /// The kind of binding this fills.
    pub const fn kind(self) -> BindingKind {
        match self {
            Self::Uniform(_) => BindingKind::UniformBuffer,
            Self::Storage(_) => BindingKind::StorageBuffer,
            Self::Sampled(..) => BindingKind::SampledImage,
            Self::Texture(_) => BindingKind::Texture,
            Self::Sampler(_) => BindingKind::Sampler,
            Self::StorageImage(_) => BindingKind::StorageImage,
        }
    }
}

/// What kind of program, with its shaders (SPIR-V words).
#[derive(Debug, Clone, Copy)]
// A draw pipeline is larger than a compute shader, and made once per
// program: not worth a box.
#[allow(clippy::large_enum_variant)]
pub enum ProgramKind<'a> {
    /// A compute shader, for [`Dispatch`].
    Compute {
        /// The shader.
        spirv: &'a [u32],
        /// Its entry point.
        entry: &'a str,
    },
    /// A vertex and a fragment shader, and how they draw, for [`Draw`].
    Draw(DrawPipeline<'a>),
}

/// Create a buffer of `size` bytes, lending it for a scope. Its contents
/// start undefined (zeros in [`SimGpu`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateBuffer {
    /// Its size, in bytes; at least 1.
    pub size: u64,
    /// What it is for.
    pub usage: BufferUsage,
}

impl CreateBuffer {
    /// Describes a buffer of `size` bytes, for `usage`.
    pub const fn new(size: u64, usage: BufferUsage) -> Self {
        Self { size, usage }
    }
}

/// What shape an image is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ImageKind {
    /// Flat, with `layers` layers (an array, if more than one).
    #[default]
    D2,
    /// Six square faces per cube (`layers` is a multiple of 6: more is an
    /// array of cubes), sampled by direction.
    Cube,
    /// A volume, `depth` slices deep (and one layer).
    D3,
}

/// Create an image, lending it for a scope. Its contents start undefined
/// (zeros in [`SimGpu`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CreateImage {
    /// Its width, in pixels; at least 1.
    pub width: u32,
    /// Its height, in pixels; at least 1.
    pub height: u32,
    /// Its depth, in slices: more than 1 only for [`ImageKind::D3`].
    pub depth: u32,
    /// Its layers: array elements, or six per cube.
    pub layers: u32,
    /// Its mip levels, each half the size of the one before; at least 1.
    pub mips: u32,
    /// Samples per pixel: 1, or 2, 4, 8, ... for a multisampled target.
    pub samples: u32,
    /// Its shape.
    pub kind: ImageKind,
    /// Its pixel format.
    pub format: Format,
    /// What it is for.
    pub usage: ImageUsage,
}

impl CreateImage {
    /// Describes a `width` by `height` image in `format`, for `usage`: one
    /// layer, one mip level, one sample.
    pub const fn new(width: u32, height: u32, format: Format, usage: ImageUsage) -> Self {
        Self {
            width,
            height,
            depth: 1,
            layers: 1,
            mips: 1,
            samples: 1,
            kind: ImageKind::D2,
            format,
            usage,
        }
    }

    /// A cube map (or `cubes` of them, as an array), each face `size`
    /// square.
    pub const fn cube(size: u32, cubes: u32, format: Format, usage: ImageUsage) -> Self {
        let mut op = Self::new(size, size, format, usage);
        op.kind = ImageKind::Cube;
        op.layers = 6 * cubes;
        op
    }

    /// A volume, `depth` slices deep.
    pub const fn volume(
        width: u32,
        height: u32,
        depth: u32,
        format: Format,
        usage: ImageUsage,
    ) -> Self {
        let mut op = Self::new(width, height, format, usage);
        op.kind = ImageKind::D3;
        op.depth = depth;
        op
    }

    /// With `layers` layers: an array.
    #[must_use]
    pub const fn layers(mut self, layers: u32) -> Self {
        self.layers = layers;
        self
    }

    /// With `mips` mip levels.
    #[must_use]
    pub const fn mips(mut self, mips: u32) -> Self {
        self.mips = mips;
        self
    }

    /// With every mip level down to 1 by 1.
    #[must_use]
    pub const fn full_mips(mut self) -> Self {
        let mut largest = if self.width > self.height {
            self.width
        } else {
            self.height
        };
        if self.depth > largest {
            largest = self.depth;
        }
        self.mips = 32 - largest.leading_zeros();
        self
    }

    /// With `samples` samples per pixel.
    #[must_use]
    pub const fn samples(mut self, samples: u32) -> Self {
        self.samples = samples;
        self
    }

    /// The width, height and depth of mip level `mip`.
    pub const fn extent(&self, mip: u32) -> (u32, u32, u32) {
        const fn at(n: u32, mip: u32) -> u32 {
            let v = n >> mip;
            if v == 0 { 1 } else { v }
        }
        (
            at(self.width, mip),
            at(self.height, mip),
            at(self.depth, mip),
        )
    }

    /// The bytes of mip level `mip`, every layer and slice, tightly
    /// packed: what [`UploadImage`] and [`DownloadImage`] move by default.
    pub const fn level_bytes(&self, mip: u32) -> u64 {
        let (w, h, d) = self.extent(mip);
        self.format.size(w, h) * d as u64 * self.layers as u64
    }
}

/// Compile a program, lending it for a scope.
#[derive(Debug, Clone, Copy)]
pub struct CreateProgram<'a> {
    /// A name for it, which [`SimGpu`] runs it by and errors mention.
    pub label: &'a str,
    /// What it is.
    pub kind: ProgramKind<'a>,
    /// Its bindings, in order: set 0, binding `i`.
    pub bindings: &'a [BindingKind],
    /// How many bytes of push constants it takes (a multiple of 4, at most
    /// 128).
    pub push_constants: u32,
    /// Each binding's array length, by binding (empty: every binding is
    /// one resource). A binding `n` long is given `n` [`Bind`]s in a row.
    pub counts: &'a [u32],
    /// Specialization constants, for every stage.
    pub constants: &'a [Constant],
}

impl<'a> CreateProgram<'a> {
    /// A compute program.
    pub const fn compute(
        label: &'a str,
        spirv: &'a [u32],
        entry: &'a str,
        bindings: &'a [BindingKind],
    ) -> Self {
        Self {
            label,
            kind: ProgramKind::Compute { spirv, entry },
            bindings,
            push_constants: 0,
            counts: &[],
            constants: &[],
        }
    }

    /// A draw program.
    pub const fn draw(label: &'a str, kind: ProgramKind<'a>, bindings: &'a [BindingKind]) -> Self {
        Self {
            label,
            kind,
            bindings,
            push_constants: 0,
            counts: &[],
            constants: &[],
        }
    }

    /// Takes `bytes` of push constants.
    #[must_use]
    pub const fn push_constants(mut self, bytes: u32) -> Self {
        self.push_constants = bytes;
        self
    }

    /// With arrays of bindings: `counts[i]` resources at binding `i`
    /// (WGSL's `binding_array<texture_2d<f32>, 8>`, GLSL's
    /// `sampler2D textures[8]`), one entry per binding.
    #[must_use]
    pub const fn counts(mut self, counts: &'a [u32]) -> Self {
        self.counts = counts;
        self
    }

    /// With specialization constants (WGSL's `override`, GLSL's
    /// `layout(constant_id = ..)`), set for every stage.
    #[must_use]
    pub const fn constants(mut self, constants: &'a [Constant]) -> Self {
        self.constants = constants;
        self
    }
}

/// A specialization constant: its ID in the shader, and its 32 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Constant {
    /// The constant's ID (WGSL's `@id(..)`, GLSL's `constant_id`).
    pub id: u32,
    /// Its value's bits.
    pub bits: u32,
}

impl Constant {
    /// An unsigned integer.
    pub const fn u32(id: u32, value: u32) -> Self {
        Self { id, bits: value }
    }

    /// A signed integer.
    pub const fn i32(id: u32, value: i32) -> Self {
        Self {
            id,
            bits: value as u32,
        }
    }

    /// A float.
    pub const fn f32(id: u32, value: f32) -> Self {
        Self {
            id,
            bits: value.to_bits(),
        }
    }

    /// A boolean.
    pub const fn bool(id: u32, value: bool) -> Self {
        Self {
            id,
            bits: value as u32,
        }
    }
}

/// Write `data` into `buffer` at `offset`.
#[derive(Debug, Clone, Copy)]
pub struct Upload<'a> {
    /// Where.
    pub buffer: Buffer,
    /// How far in, in bytes.
    pub offset: u64,
    /// What.
    pub data: &'a [u8],
}

impl<'a> Upload<'a> {
    /// Writes `data` at the start of `buffer`.
    pub const fn new(buffer: Buffer, data: &'a [u8]) -> Self {
        Self {
            buffer,
            offset: 0,
            data,
        }
    }

    /// Writes `offset` bytes in.
    #[must_use]
    pub const fn at(mut self, offset: u64) -> Self {
        self.offset = offset;
        self
    }
}

/// A part of an image: a box of one mip level, across a run of layers.
/// [`ALL`](Self::ALL) is every layer of level 0, the whole of it; the
/// builders narrow it. Its bytes, as uploads and downloads move them, are
/// each layer in turn, each slice in turn, each row top to bottom, tightly
/// packed (whole blocks, for a compressed format).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageRegion {
    /// The mip level.
    pub mip: u32,
    /// The first layer.
    pub layer: u32,
    /// How many layers; [`ImageRegion::REST`] for the rest of them.
    pub layers: u32,
    /// The box's left, top and front, in pixels of the level.
    pub offset: [u32; 3],
    /// Its width, height and depth; [`ImageRegion::REST`] for the rest of
    /// the level.
    pub extent: [u32; 3],
}

impl ImageRegion {
    /// To the edge: the rest of the layers, or of the level.
    pub const REST: u32 = u32::MAX;

    /// The whole of level 0, every layer.
    pub const ALL: Self = Self {
        mip: 0,
        layer: 0,
        layers: Self::REST,
        offset: [0; 3],
        extent: [Self::REST; 3],
    };

    /// The whole of level `mip`, every layer.
    pub const fn level(mip: u32) -> Self {
        let mut r = Self::ALL;
        r.mip = mip;
        r
    }

    /// Only `count` layers from `first`.
    #[must_use]
    pub const fn layers(mut self, first: u32, count: u32) -> Self {
        self.layer = first;
        self.layers = count;
        self
    }

    /// Only the `width` by `height` rectangle at `x`, `y` (every slice).
    #[must_use]
    pub const fn rect(mut self, x: u32, y: u32, width: u32, height: u32) -> Self {
        self.offset = [x, y, 0];
        self.extent = [width, height, Self::REST];
        self
    }

    /// Only the box at `offset`, `extent` big: for volumes.
    #[must_use]
    pub const fn boxed(mut self, offset: [u32; 3], extent: [u32; 3]) -> Self {
        self.offset = offset;
        self.extent = extent;
        self
    }

    /// The region within `image`, [`REST`](Self::REST) resolved: its
    /// layers (or, for a volume, 1), and its extent.
    pub const fn resolve(&self, image: &CreateImage) -> (u32, [u32; 3]) {
        let (w, h, d) = image.extent(self.mip);
        const fn rest(n: u32, size: u32, from: u32) -> u32 {
            if n == ImageRegion::REST {
                size.saturating_sub(from)
            } else {
                n
            }
        }
        let layers = rest(self.layers, image.layers, self.layer);
        let extent = [
            rest(self.extent[0], w, self.offset[0]),
            rest(self.extent[1], h, self.offset[1]),
            rest(self.extent[2], d, self.offset[2]),
        ];
        (layers, extent)
    }

    /// The bytes of the region within `image`, tightly packed.
    pub const fn bytes(&self, image: &CreateImage) -> u64 {
        let (layers, [w, h, d]) = self.resolve(image);
        image.format.size(w, h) * d as u64 * layers as u64
    }
}

/// Write `data` into a part of `image` (all of level 0 by default): its
/// bytes laid out as [`ImageRegion`] says.
#[derive(Debug, Clone, Copy)]
pub struct UploadImage<'a> {
    /// Where.
    pub image: Image,
    /// Which part.
    pub region: ImageRegion,
    /// What: the region's bytes.
    pub data: &'a [u8],
}

impl<'a> UploadImage<'a> {
    /// Writes `data` into all of `image`'s level 0.
    pub const fn new(image: Image, data: &'a [u8]) -> Self {
        Self {
            image,
            region: ImageRegion::ALL,
            data,
        }
    }

    /// Into `region` instead.
    #[must_use]
    pub const fn region(mut self, region: ImageRegion) -> Self {
        self.region = region;
        self
    }
}

/// Where in an image a copy starts: a mip level, a first layer, and a
/// pixel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ImageAt {
    /// The mip level.
    pub mip: u32,
    /// The first layer.
    pub layer: u32,
    /// The pixel: left, top, front.
    pub offset: [u32; 3],
}

impl ImageAt {
    /// Pixel `offset` of level `mip`, from layer `layer`.
    pub const fn new(mip: u32, layer: u32, offset: [u32; 3]) -> Self {
        Self { mip, layer, offset }
    }
}

/// Copy a box of pixels, across `layers` layers, from one image to
/// another of the same format (or another of as many bytes per pixel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyImage {
    /// From.
    pub src: Image,
    /// Where in `src`.
    pub src_at: ImageAt,
    /// To: a different image.
    pub dst: Image,
    /// Where in `dst`.
    pub dst_at: ImageAt,
    /// The box's width, height and depth.
    pub extent: [u32; 3],
    /// How many layers.
    pub layers: u32,
}

impl CopyImage {
    /// Copies all of `src`'s level 0 into `dst`'s level 0, every layer:
    /// the two the same size.
    pub const fn whole(src: Image, dst: Image, width: u32, height: u32) -> Self {
        Self {
            src,
            src_at: ImageAt::new(0, 0, [0; 3]),
            dst,
            dst_at: ImageAt::new(0, 0, [0; 3]),
            extent: [width, height, 1],
            layers: 1,
        }
    }
}

/// Copy a rectangle of one image into a rectangle of another, scaling it
/// with `filter`: one layer of one level each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlitImage {
    /// From.
    pub src: Image,
    /// Its level.
    pub src_mip: u32,
    /// Its layer.
    pub src_layer: u32,
    /// The rectangle read: left, top, right, bottom (exclusive).
    pub src_rect: [u32; 4],
    /// To: a different image.
    pub dst: Image,
    /// Its level.
    pub dst_mip: u32,
    /// Its layer.
    pub dst_layer: u32,
    /// The rectangle written.
    pub dst_rect: [u32; 4],
    /// How pixels between source pixels are read.
    pub filter: Filter,
}

/// Fill every mip level of `image` but the first from the one before it,
/// halving each way with linear filtering: a mip chain from level 0. Every
/// layer is filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerateMips {
    /// The image: more than one mip level, in a format that filters.
    pub image: Image,
}

/// Fill `size` bytes of `buffer` from `offset` (both multiples of 4) with
/// `value`, repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillBuffer {
    /// The buffer.
    pub buffer: Buffer,
    /// Where, in bytes.
    pub offset: u64,
    /// How much, in bytes.
    pub size: u64,
    /// The four bytes written, as a little-endian `u32`.
    pub value: u32,
}

/// Copy `size` bytes from one buffer to another (or within one, if the
/// ranges do not overlap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyBuffer {
    /// From.
    pub src: Buffer,
    /// How far into `src`.
    pub src_offset: u64,
    /// To.
    pub dst: Buffer,
    /// How far into `dst`.
    pub dst_offset: u64,
    /// How much.
    pub size: u64,
}

/// Run a compute program over `groups` workgroups.
#[derive(Debug, Clone, Copy)]
pub struct Dispatch<'a> {
    /// The program.
    pub program: Program,
    /// Its bindings, in order.
    pub bindings: &'a [Bind],
    /// Its push constants.
    pub push: &'a [u8],
    /// How many workgroups, in x, y and z.
    pub groups: [u32; 3],
}

impl<'a> Dispatch<'a> {
    /// Runs `program` with `bindings` over `groups`.
    pub const fn new(program: Program, bindings: &'a [Bind], groups: [u32; 3]) -> Self {
        Self {
            program,
            bindings,
            push: &[],
            groups,
        }
    }

    /// With push constants.
    #[must_use]
    pub const fn push(mut self, push: &'a [u8]) -> Self {
        self.push = push;
        self
    }
}

/// What a set of [`Queries`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryKind {
    /// When the GPU got to a point in its work ([`WriteTimestamp`]), on a
    /// device with [`GpuFeatures::TIMESTAMPS`].
    Timestamp,
    /// How many samples of a draw passed its depth and stencil tests
    /// ([`Draw::occlusion`]): or, without
    /// [`GpuFeatures::PRECISE_OCCLUSION`], only whether any did (a count
    /// above 0).
    Occlusion,
}

/// Create `count` query slots of one kind, lending them for a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateQueries {
    /// What they record.
    pub kind: QueryKind,
    /// How many; at least 1.
    pub count: u32,
}

impl CreateQueries {
    /// `count` slots for `kind`.
    pub const fn new(kind: QueryKind, count: u32) -> Self {
        Self { kind, count }
    }
}

/// Record in slot `index` of timestamp queries when the GPU has done
/// everything performed before this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteTimestamp {
    /// The queries.
    pub queries: Queries,
    /// The slot.
    pub index: u32,
}

/// Wait for the work that wrote them, and read `out.len()` query slots
/// from `first`: timestamps in nanoseconds (from a point of the device's
/// own), occlusion counts in samples. Every slot read has been written.
#[derive(Debug)]
pub struct ReadQueries<'a> {
    /// The queries.
    pub queries: Queries,
    /// The first slot.
    pub first: u32,
    /// Where the results go.
    pub out: &'a mut [u64],
}

impl<'a> ReadQueries<'a> {
    /// Reads slots from 0 into `out`.
    pub fn new(queries: Queries, out: &'a mut [u64]) -> Self {
        Self {
            queries,
            first: 0,
            out,
        }
    }
}

/// A buffer's address on the GPU, made with [`BufferUsage::ADDRESS`]: for
/// a shader to read and write it through (GLSL's `buffer_reference`,
/// SPIR-V's physical storage buffers), given in a push constant or
/// another buffer. A program may reach any such buffer this way, so
/// every one is ordered against every dispatch and draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferAddress(pub Buffer);

/// Run a compute program over as many workgroups as a buffer says: three
/// `u32`s at `offset` (a multiple of 4) of a buffer made with
/// [`BufferUsage::INDIRECT`], written there by an upload or a program.
#[derive(Debug, Clone, Copy)]
pub struct DispatchIndirect<'a> {
    /// The program.
    pub program: Program,
    /// Its bindings, in order.
    pub bindings: &'a [Bind],
    /// Its push constants.
    pub push: &'a [u8],
    /// The buffer holding the counts.
    pub buffer: Buffer,
    /// Where in it.
    pub offset: u64,
}

impl<'a> DispatchIndirect<'a> {
    /// Runs `program` with `bindings` over the workgroups at `offset` of
    /// `buffer`.
    pub const fn new(program: Program, bindings: &'a [Bind], buffer: Buffer, offset: u64) -> Self {
        Self {
            program,
            bindings,
            push: &[],
            buffer,
            offset,
        }
    }

    /// With push constants.
    #[must_use]
    pub const fn push(mut self, push: &'a [u8]) -> Self {
        self.push = push;
        self
    }
}

/// Where an indirect draw's counts are: `count` commands, `stride` bytes
/// apart, from `offset` (a multiple of 4) of a buffer made with
/// [`BufferUsage::INDIRECT`]. Each is four `u32`s (vertex count, instance
/// count, first vertex, first instance), or for an indexed draw five
/// (index count, instance count, first index, an `i32` base vertex,
/// first instance): Vulkan's `VkDrawIndirectCommand` and
/// `VkDrawIndexedIndirectCommand`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Indirect {
    /// The buffer.
    pub buffer: Buffer,
    /// Where the first command is.
    pub offset: u64,
    /// How many commands.
    pub count: u32,
    /// The bytes from one to the next: at least a command's size, a
    /// multiple of 4; or 0, for a command's size.
    pub stride: u32,
}

impl Indirect {
    /// A command's bytes, indexed or not.
    pub const fn command_size(indexed: bool) -> u32 {
        if indexed { 20 } else { 16 }
    }

    /// The bytes from one command to the next.
    pub const fn step(&self, indexed: bool) -> u32 {
        if self.stride == 0 {
            Self::command_size(indexed)
        } else {
            self.stride
        }
    }
}

/// Draw with a draw program: `vertex_count` vertices (or indices, with an
/// index buffer), `instance_count` times, into its targets.
#[derive(Debug, Clone, Copy)]
pub struct Draw<'a> {
    /// The program.
    pub program: Program,
    /// Its colour targets, by the program's slots.
    pub targets: [Option<Attachment>; MAX_TARGETS],
    /// Its depth (and stencil) image, if the program tests depth.
    pub depth: Option<DepthAttachment>,
    /// Its vertex buffers, by the program's slots.
    pub vertex_buffers: [Option<VertexBinding>; MAX_VERTEX_BUFFERS],
    /// Its index buffer, for an indexed draw.
    pub index: Option<IndexBuffer>,
    /// Counts from a buffer, in place of `vertex_count`, `instance_count`,
    /// `first`, `base_vertex` and `first_instance`.
    pub indirect: Option<Indirect>,
    /// An occlusion query slot to count the draw's passing samples in.
    pub occlusion: Option<(Queries, u32)>,
    /// How many vertices, or indices.
    pub vertex_count: u32,
    /// How many instances.
    pub instance_count: u32,
    /// The first vertex, or index.
    pub first: u32,
    /// For an indexed draw: added to each index.
    pub base_vertex: i32,
    /// The first instance.
    pub first_instance: u32,
    /// Where it lands in the targets; all of them if `None`.
    pub viewport: Option<Viewport>,
    /// What it may write; all of the targets if `None`.
    pub scissor: Option<Rect>,
    /// The stencil test's reference value.
    pub stencil_reference: u32,
    /// Its bindings, in order.
    pub bindings: &'a [Bind],
    /// Its push constants.
    pub push: &'a [u8],
}

impl<'a> Draw<'a> {
    /// Draws `vertex_count` vertices of `program` into `target` (its slot
    /// 0), on top of what it holds.
    pub const fn new(program: Program, target: Image, vertex_count: u32) -> Self {
        let mut targets = [None; MAX_TARGETS];
        targets[0] = Some(Attachment::new(target));
        Self::with_targets(program, targets, vertex_count)
    }

    /// Draws `vertex_count` vertices of `program` into `targets`.
    pub const fn with_targets(
        program: Program,
        targets: [Option<Attachment>; MAX_TARGETS],
        vertex_count: u32,
    ) -> Self {
        Self {
            program,
            targets,
            depth: None,
            vertex_buffers: [None; MAX_VERTEX_BUFFERS],
            index: None,
            indirect: None,
            occlusion: None,
            vertex_count,
            instance_count: 1,
            first: 0,
            base_vertex: 0,
            first_instance: 0,
            viewport: None,
            scissor: None,
            stencil_reference: 0,
            bindings: &[],
            push: &[],
        }
    }

    /// Clears target 0 to `rgba` first.
    #[must_use]
    pub const fn clear(mut self, rgba: [f32; 4]) -> Self {
        if let Some(t) = &mut self.targets[0] {
            t.load = Load::Clear(rgba);
        }
        self
    }

    /// Sets colour target `slot`.
    #[must_use]
    pub const fn target(mut self, slot: usize, target: Option<Attachment>) -> Self {
        self.targets[slot] = target;
        self
    }

    /// Tests depth against `depth`.
    #[must_use]
    pub const fn depth(mut self, depth: DepthAttachment) -> Self {
        self.depth = Some(depth);
        self
    }

    /// Reads vertex buffer 0 from `buffer`.
    #[must_use]
    pub const fn vertices(self, buffer: Buffer) -> Self {
        self.vertex_buffer(0, buffer, 0)
    }

    /// Reads vertex buffer `slot` from `buffer`, `offset` bytes in.
    #[must_use]
    pub const fn vertex_buffer(mut self, slot: usize, buffer: Buffer, offset: u64) -> Self {
        self.vertex_buffers[slot] = Some(VertexBinding { buffer, offset });
        self
    }

    /// Draws by the indices in `buffer`: `vertex_count` of them.
    #[must_use]
    pub const fn indexed(mut self, buffer: Buffer, format: IndexFormat) -> Self {
        self.index = Some(IndexBuffer {
            buffer,
            offset: 0,
            format,
        });
        self
    }

    /// Counts the samples that pass in slot `index` of occlusion
    /// `queries`.
    #[must_use]
    pub const fn occlusion(mut self, queries: Queries, index: u32) -> Self {
        self.occlusion = Some((queries, index));
        self
    }

    /// Draws what the command at `offset` of `buffer` says: see
    /// [`Indirect`].
    #[must_use]
    pub const fn indirect(self, buffer: Buffer, offset: u64) -> Self {
        self.indirect_many(buffer, offset, 1, 0)
    }

    /// Draws `count` commands from `offset` of `buffer`, `stride` bytes
    /// apart (0: one after another), on a device with
    /// [`GpuFeatures::MULTI_DRAW_INDIRECT`] for more than one.
    #[must_use]
    pub const fn indirect_many(
        mut self,
        buffer: Buffer,
        offset: u64,
        count: u32,
        stride: u32,
    ) -> Self {
        self.indirect = Some(Indirect {
            buffer,
            offset,
            count,
            stride,
        });
        self
    }

    /// Draws `count` instances.
    #[must_use]
    pub const fn instances(mut self, count: u32) -> Self {
        self.instance_count = count;
        self
    }

    /// Starts at vertex (or index) `first`.
    #[must_use]
    pub const fn first(mut self, first: u32) -> Self {
        self.first = first;
        self
    }

    /// Lands in `viewport`.
    #[must_use]
    pub const fn viewport(mut self, viewport: Viewport) -> Self {
        self.viewport = Some(viewport);
        self
    }

    /// Writes only within `scissor`.
    #[must_use]
    pub const fn scissor(mut self, scissor: Rect) -> Self {
        self.scissor = Some(scissor);
        self
    }

    /// With stencil reference `reference`.
    #[must_use]
    pub const fn stencil_reference(mut self, reference: u32) -> Self {
        self.stencil_reference = reference;
        self
    }

    /// With bindings.
    #[must_use]
    pub const fn bind(mut self, bindings: &'a [Bind]) -> Self {
        self.bindings = bindings;
        self
    }

    /// With push constants.
    #[must_use]
    pub const fn push(mut self, push: &'a [u8]) -> Self {
        self.push = push;
        self
    }
}

/// Read `out.len()` bytes of `buffer` from `offset`, waiting for the work
/// before it.
#[derive(Debug)]
pub struct Download<'a> {
    /// From.
    pub buffer: Buffer,
    /// How far in.
    pub offset: u64,
    /// Into.
    pub out: &'a mut [u8],
}

impl<'a> Download<'a> {
    /// Reads the start of `buffer` into `out`.
    pub fn new(buffer: Buffer, out: &'a mut [u8]) -> Self {
        Self {
            buffer,
            offset: 0,
            out,
        }
    }

    /// Reads from `offset` bytes in.
    #[must_use]
    pub fn at(mut self, offset: u64) -> Self {
        self.offset = offset;
        self
    }
}

/// Read a part of `image` (all of level 0 by default), waiting for the
/// work before it: its bytes laid out as [`ImageRegion`] says.
#[derive(Debug)]
pub struct DownloadImage<'a> {
    /// From.
    pub image: Image,
    /// Which part.
    pub region: ImageRegion,
    /// Into: the region's bytes.
    pub out: &'a mut [u8],
}

impl<'a> DownloadImage<'a> {
    /// Reads all of `image`'s level 0 into `out`.
    pub fn new(image: Image, out: &'a mut [u8]) -> Self {
        Self {
            image,
            region: ImageRegion::ALL,
            out,
        }
    }

    /// Reads `region` instead.
    #[must_use]
    pub fn region(mut self, region: ImageRegion) -> Self {
        self.region = region;
        self
    }
}

/// Send everything performed so far to the GPU without waiting, lending a
/// submission of type `S` for a scope, to inspect [`IsDone`] on. Closing
/// the scope is the wait: the work has finished once the scope returns; a scope that fails or is cancelled lets go
/// without waiting, and the work runs to its end on its own (GPU work
/// cannot be stopped).
///
/// With nothing performed since the last submission, it lends the last
/// one, or one already done.
pub struct Submit<S> {
    submission: core::marker::PhantomData<fn() -> S>,
}

impl<S> Submit<S> {
    /// Describes submitting.
    pub const fn new() -> Self {
        Self {
            submission: core::marker::PhantomData,
        }
    }
}

impl<S> Default for Submit<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> core::fmt::Debug for Submit<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Submit")
    }
}

impl<S> Acquisition for Submit<S> {
    type Resource = S;
    type Error = GpuError;
}

/// Use an image that lives outside this handler (a Wayland client's
/// buffer, a texture another API shares) for a scope, without copying it
/// on the CPU, lending it as an [`Image`] like [`CreateImage`] does.
///
/// `B` describes it, by platform: [`drm::Dmabuf`] on Linux and the BSDs,
/// [`win32::SharedHandle`] on Windows, [`metal::IoSurface`] on Apple
/// systems; a handler implements `Import<&B>` for the kinds it takes.
/// How it gets the image onto the GPU is the handler's business.
///
/// Opening waits until whoever writes the image has finished, where the
/// platform says (a dmabuf's implicit fences); closing waits until the GPU
/// no longer uses it, after which its owner may change or reuse it (for a
/// Wayland client, release the buffer).
///
#[cfg_attr(
    not(drm),
    doc = "[`drm::Dmabuf`]: https://docs.rs/seam-rs-ash/latest/seam_ash/drm/struct.Dmabuf.html"
)]
#[cfg_attr(
    not(windows),
    doc = "[`win32::SharedHandle`]: https://docs.rs/seam-rs-ash/latest/x86_64-pc-windows-msvc/seam_ash/win32/struct.SharedHandle.html"
)]
#[cfg_attr(
    not(target_vendor = "apple"),
    doc = "[`metal::IoSurface`]: https://docs.rs/seam-rs-ash/latest/x86_64-apple-darwin/seam_ash/metal/struct.IoSurface.html"
)]
#[derive(Debug, Clone, Copy)]
pub struct Import<B> {
    /// What to import.
    pub source: B,
    /// What it is for: [`ImageUsage::SAMPLED`] unless set.
    pub usage: ImageUsage,
}

impl<B> Import<B> {
    /// Describes importing `source`, to sample.
    pub const fn new(source: B) -> Self {
        Self {
            source,
            usage: ImageUsage::SAMPLED,
        }
    }

    /// Sets what it is for.
    #[must_use]
    pub const fn usage(mut self, usage: ImageUsage) -> Self {
        self.usage = usage;
        self
    }
}

/// Whether a submission's work has finished, without waiting. Inspected on
/// the submission.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IsDone;

/// Submit everything performed so far and wait for it to finish.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Finish;

/// Which GPU this is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceInfo;

/// What [`DeviceInfo`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    /// The device's name.
    pub name: String,
    /// What kind of device.
    pub kind: GpuKind,
    /// The Vulkan version it supports, as `(major, minor, patch)`.
    pub api_version: (u32, u32, u32),
    /// Its optional features.
    pub features: GpuFeatures,
    /// How far it goes.
    pub limits: Limits,
}

/// How far a device goes: sizes it handles. What goes further fails, in
/// Vulkan (the validation layer says so) rather than in this crate's
/// checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Limits {
    /// The largest workgroup, each way.
    pub workgroup_size: [u32; 3],
    /// The most invocations in a workgroup.
    pub workgroup_invocations: u32,
    /// The most workgroups in a dispatch, each way.
    pub workgroups: [u32; 3],
    /// Workgroup memory (WGSL's `var<workgroup>`), in bytes.
    pub workgroup_memory: u32,
    /// Push constants, in bytes.
    pub push_constants: u32,
    /// The widest and tallest flat image.
    pub image_2d: u32,
    /// The widest, tallest and deepest volume.
    pub image_3d: u32,
    /// The largest cube face.
    pub image_cube: u32,
    /// The most layers in an image.
    pub layers: u32,
    /// The most bytes a storage binding reaches.
    pub storage_buffer_range: u32,
    /// The most bytes a uniform binding reaches.
    pub uniform_buffer_range: u32,
    /// The most resources one stage of a program binds, every element of
    /// an array counted.
    pub bindings_per_stage: u32,
    /// The most samples per pixel of a colour target.
    pub samples: u32,
    /// How many invocations a subgroup has.
    pub subgroup_size: u32,
    /// What subgroups do in compute shaders.
    pub subgroup_ops: SubgroupOps,
}

impl Limits {
    /// What every Vulkan 1.3 device reaches: a program within these runs
    /// anywhere. What [`SimGpu`] reports.
    pub const MINIMUM: Self = Self {
        workgroup_size: [128, 128, 64],
        workgroup_invocations: 128,
        workgroups: [65535; 3],
        workgroup_memory: 16384,
        push_constants: 128,
        image_2d: 4096,
        image_3d: 256,
        image_cube: 4096,
        layers: 256,
        storage_buffer_range: 1 << 27,
        uniform_buffer_range: 16384,
        bindings_per_stage: 128,
        samples: 4,
        subgroup_size: 1,
        subgroup_ops: SubgroupOps::BASIC,
    };
}

/// What kind of GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuKind {
    /// A separate GPU.
    Discrete,
    /// A GPU built into the CPU.
    Integrated,
    /// A virtual machine's GPU.
    Virtual,
    /// Software, on the CPU (llvmpipe, SwiftShader).
    Cpu,
    /// Anything else, including [`SimGpu`].
    Other,
}

/// Why a GPU operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuError {
    /// The GPU or the host ran out of memory.
    OutOfMemory,
    /// The device was lost (a crash, a reset, a removed GPU). Nothing more
    /// can be done with it.
    DeviceLost,
    /// The operation was wrong: an unknown or destroyed resource, a range
    /// outside it, a missing usage, a binding that does not match.
    Invalid(String),
    /// The device cannot do it.
    Unsupported(String),
    /// The window has changed: [`ResizeSwapchain`] before the next frame.
    Outdated,
    /// Vulkan failed with this result code.
    Vulkan(i32),
}

impl core::fmt::Display for GpuError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutOfMemory => f.write_str("out of GPU memory"),
            Self::DeviceLost => f.write_str("the GPU device was lost"),
            Self::Invalid(why) => write!(f, "invalid GPU operation: {why}"),
            Self::Unsupported(why) => write!(f, "not supported by the GPU: {why}"),
            Self::Outdated => f.write_str("the window has changed: resize the swapchain"),
            Self::Vulkan(code) => write!(f, "Vulkan error {code}"),
        }
    }
}

impl std::error::Error for GpuError {}

pub(crate) fn invalid(why: impl Into<String>) -> GpuError {
    GpuError::Invalid(why.into())
}

macro_rules! outcomes {
    ($($op:ty => $out:ty;)*) => {$(
        impl Operation for $op {
            type Outcome = $out;
        }
    )*};
}

outcomes! {
    Upload<'_> => Result<(), GpuError>;
    UploadImage<'_> => Result<(), GpuError>;
    CopyBuffer => Result<(), GpuError>;
    CopyImage => Result<(), GpuError>;
    BlitImage => Result<(), GpuError>;
    GenerateMips => Result<(), GpuError>;
    FillBuffer => Result<(), GpuError>;
    WriteTimestamp => Result<(), GpuError>;
    ReadQueries<'_> => Result<(), GpuError>;
    BufferAddress => Result<u64, GpuError>;
    NextFrame => Result<Image, GpuError>;
    Present => Result<(), GpuError>;
    ResizeSwapchain => Result<(), GpuError>;
    Dispatch<'_> => Result<(), GpuError>;
    DispatchIndirect<'_> => Result<(), GpuError>;
    Draw<'_> => Result<(), GpuError>;
    Download<'_> => Result<(), GpuError>;
    DownloadImage<'_> => Result<(), GpuError>;
    Finish => Result<(), GpuError>;
    IsDone => Result<bool, GpuError>;
    DeviceInfo => GpuInfo;
}

impl Query for DeviceInfo {}
impl Query for IsDone {}

impl Acquisition for CreateBuffer {
    type Resource = Buffer;
    type Error = GpuError;
}

impl Acquisition for CreateImage {
    type Resource = Image;
    type Error = GpuError;
}

impl<S> Effect for Submit<S> {
    type Key = effects::Submit;
}

impl<B> Acquisition for Import<B> {
    type Resource = Image;
    type Error = GpuError;
}

impl<B> Effect for Import<B> {
    type Key = effects::Import;
}

impl Acquisition for CreateSwapchain {
    type Resource = Swapchain;
    type Error = GpuError;
}

impl Acquisition for CreateQueries {
    type Resource = Queries;
    type Error = GpuError;
}

impl Acquisition for CreateSampler {
    type Resource = Sampler;
    type Error = GpuError;
}

impl Acquisition for CreateProgram<'_> {
    type Resource = Program;
    type Error = GpuError;
}

/// The keys that route this crate's effects in a root.
pub mod effects {
    macro_rules! keys {
        ($($name:ident),*) => {$(
            #[doc = concat!("Routes [`", stringify!($name), "`](crate::", stringify!($name), ").")]
            #[derive(Debug, Clone, Copy)]
            pub struct $name;
        )*};
    }
    keys!(
        CreateBuffer,
        CreateImage,
        CreateSampler,
        CreateQueries,
        CreateSwapchain,
        CreateProgram,
        Upload,
        UploadImage,
        CopyBuffer,
        CopyImage,
        BlitImage,
        GenerateMips,
        FillBuffer,
        WriteTimestamp,
        ReadQueries,
        BufferAddress,
        NextFrame,
        Present,
        ResizeSwapchain,
        Dispatch,
        DispatchIndirect,
        Draw,
        Download,
        DownloadImage,
        Finish,
        DeviceInfo,
        Submit,
        Import
    );
    /// Routes [`DmabufFormats`](crate::drm::DmabufFormats).
    #[cfg(drm)]
    #[derive(Debug, Clone, Copy)]
    pub struct DmabufFormats;
}

macro_rules! keyed {
    ($($op:ty => $key:ident;)*) => {$(
        impl Effect for $op {
            type Key = effects::$key;
        }
    )*};
}

keyed! {
    CreateBuffer => CreateBuffer;
    CreateImage => CreateImage;
    CreateSampler => CreateSampler;
    CreateQueries => CreateQueries;
    WriteTimestamp => WriteTimestamp;
    ReadQueries<'_> => ReadQueries;
    BufferAddress => BufferAddress;
    CreateSwapchain => CreateSwapchain;
    NextFrame => NextFrame;
    Present => Present;
    ResizeSwapchain => ResizeSwapchain;
    CreateProgram<'_> => CreateProgram;
    Upload<'_> => Upload;
    UploadImage<'_> => UploadImage;
    CopyBuffer => CopyBuffer;
    CopyImage => CopyImage;
    BlitImage => BlitImage;
    GenerateMips => GenerateMips;
    FillBuffer => FillBuffer;
    Dispatch<'_> => Dispatch;
    DispatchIndirect<'_> => DispatchIndirect;
    Draw<'_> => Draw;
    Download<'_> => Download;
    DownloadImage<'_> => DownloadImage;
    Finish => Finish;
    DeviceInfo => DeviceInfo;
}

/// What a handler knows of a draw program, for checking draws against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DrawInfo {
    pub targets: [Option<Format>; MAX_TARGETS],
    pub depth: Option<Format>,
    pub stencil: bool,
    /// Each vertex buffer's stride and step.
    pub vertex_buffers: [Option<(u32, Step)>; MAX_VERTEX_BUFFERS],
    pub samples: u32,
    pub topology: Topology,
}

impl DrawInfo {
    pub(crate) fn of(p: &DrawPipeline<'_>) -> Self {
        Self {
            targets: p.targets.map(|t| t.map(|t| t.format)),
            depth: p.depth.map(|d| d.format),
            stencil: p.depth.is_some_and(|d| d.stencil.is_some()),
            vertex_buffers: p.vertex_buffers.map(|v| v.map(|v| (v.stride, v.step))),
            samples: p.samples,
            topology: p.raster.topology,
        }
    }
}

/// Bytes of a `u32` slice, little-endian: for uploading numbers.
pub fn u32_bytes(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Bytes of an `f32` slice, little-endian: for uploading vertices and
/// uniforms.
pub fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// The `u32`s in little-endian `bytes` (a trailing partial word is
/// dropped).
pub fn bytes_u32(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// The `f32`s in little-endian `bytes` (a trailing partial word is
/// dropped).
pub fn bytes_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// What a handler knows of a resource, for checking operations against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResInfo {
    Buffer {
        size: u64,
        usage: BufferUsage,
    },
    Image(CreateImage),
    Sampler(CreateSampler),
    Swapchain {
        width: u32,
        height: u32,
        format: Format,
        usage: ImageUsage,
        /// The image lent by `NextFrame`, until `Present`: a new ID each
        /// frame.
        lent: Option<u64>,
    },
    Queries {
        kind: QueryKind,
        /// Which slots have been written.
        written: Vec<bool>,
    },
    Program {
        label: String,
        compute: bool,
        /// Each binding's kind, an array's once per element.
        bindings: Vec<BindingKind>,
        /// Each of those: its binding and its element.
        slots: Vec<(u32, u32)>,
        push_constants: u32,
        constants: Vec<Constant>,
        /// For a draw program.
        draw: Option<DrawInfo>,
    },
}
