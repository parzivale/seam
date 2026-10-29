//! Vulkan for seam: the GPU as effects.
//!
//! A core sees the GPU as a few kinds of thing, and nothing else:
//!
//! - **Resources**, as scopes: [`CreateBuffer`], [`CreateImage`] and
//!   [`CreateProgram`] lend a [`Buffer`], an [`Image`] or a [`Program`]
//!   (plain IDs, the same for every handler) and destroy it when the scope
//!   closes.
//! - **Moving data**: [`Upload`], [`UploadImage`] and [`CopyBuffer`] put
//!   bytes in, [`Download`] and [`DownloadImage`] take them out.
//! - **Running programs**: [`Dispatch`] runs a compute program, [`Draw`]
//!   draws into an image, each with the resources it reads and writes
//!   bound in order.
//! - **Waiting**: [`Download`], [`DownloadImage`] and [`Finish`] wait for
//!   the work before them; nothing else does.
//! - **Submitting without waiting**: [`Submit`] is a scope that sends the
//!   work so far to the GPU and lends a submission: [`IsDone`] says
//!   whether it has finished, and closing the scope waits for it. So the CPU does other work while the GPU runs.
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
#![cfg_attr(not(feature = "ash"), forbid(unsafe_code))]

use std::string::String;
use std::vec::Vec;

use seam_core::{Acquisition, Effect, Operation, Query};

#[cfg(feature = "ash")]
#[allow(unsafe_code)] // Vulkan: every call is unsafe; see the module's docs.
mod device;
mod sim;
mod validate;

#[cfg(feature = "ash")]
pub use device::{Ash, AshOptions, Submission};

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
}

macro_rules! flags {
    ($($(#[$m:meta])* $name:ident { $($(#[$fm:meta])* $flag:ident = $bit:expr;)* })*) => {$(
        $(#[$m])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
        pub struct $name(u32);

        impl $name {
            $($(#[$fm])* pub const $flag: Self = Self(1 << $bit);)*

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
        /// A uniform buffer, bound with [`Bind::Uniform`].
        UNIFORM = 1;
        /// A storage buffer, bound with [`Bind::Storage`].
        STORAGE = 2;
    }

    /// What an image is for, beyond moving data in and out (always allowed).
    ImageUsage {
        /// Sampled by a program, bound with [`Bind::Sampled`].
        SAMPLED = 0;
        /// Drawn into, as the target of [`Draw`].
        TARGET = 1;
        /// Read and written by a program, bound with [`Bind::StorageImage`].
        STORAGE = 2;
    }
}

/// An image's pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// 8-bit red, green, blue, alpha, in [0, 1].
    Rgba8Unorm,
    /// 8-bit blue, green, red, alpha, in [0, 1]: what displays scan out.
    Bgra8Unorm,
    /// [`Rgba8Unorm`](Self::Rgba8Unorm), stored as sRGB.
    Rgba8Srgb,
    /// [`Bgra8Unorm`](Self::Bgra8Unorm), stored as sRGB.
    Bgra8Srgb,
    /// One 8-bit channel, in [0, 1].
    R8Unorm,
    /// 16-bit float red, green, blue, alpha.
    Rgba16Float,
    /// 32-bit float red, green, blue, alpha.
    Rgba32Float,
}

impl Format {
    /// Bytes per pixel.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::R8Unorm => 1,
            Self::Rgba8Unorm | Self::Bgra8Unorm | Self::Rgba8Srgb | Self::Bgra8Srgb => 4,
            Self::Rgba16Float => 8,
            Self::Rgba32Float => 16,
        }
    }
}

/// What a program's binding is, at set 0, binding `i` for the `i`th kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingKind {
    /// A uniform buffer.
    UniformBuffer,
    /// A storage buffer.
    StorageBuffer,
    /// A sampled image (a combined image sampler: linear filtering, clamped
    /// to the edge).
    SampledImage,
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
    /// An image the program samples.
    Sampled(Image),
    /// An image the program may read and write.
    StorageImage(Image),
}

impl Bind {
    /// The kind of binding this fills.
    pub const fn kind(self) -> BindingKind {
        match self {
            Self::Uniform(_) => BindingKind::UniformBuffer,
            Self::Storage(_) => BindingKind::StorageBuffer,
            Self::Sampled(_) => BindingKind::SampledImage,
            Self::StorageImage(_) => BindingKind::StorageImage,
        }
    }
}

/// The format of one vertex attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttrFormat {
    /// One `f32`.
    F32,
    /// Two `f32`s.
    F32x2,
    /// Three `f32`s.
    F32x3,
    /// Four `f32`s.
    F32x4,
    /// One `u32`.
    U32,
}

/// One vertex attribute: at location `i` for the `i`th, `offset` bytes
/// into each vertex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VertexAttr {
    /// Where it starts in the vertex.
    pub offset: u32,
    /// What it is.
    pub format: AttrFormat,
}

/// How a draw combines with what the target holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Blend {
    /// Replace it.
    #[default]
    Replace,
    /// Premultiplied alpha: `src + dst * (1 - src.a)`, as compositors
    /// blend.
    PremultipliedAlpha,
}

/// What kind of program, with its shaders (SPIR-V words).
#[derive(Debug, Clone, Copy)]
pub enum ProgramKind<'a> {
    /// A compute shader, for [`Dispatch`].
    Compute {
        /// The shader.
        spirv: &'a [u32],
        /// Its entry point.
        entry: &'a str,
    },
    /// A vertex and a fragment shader drawing triangles, for [`Draw`].
    Draw {
        /// The vertex shader.
        vertex: &'a [u32],
        /// Its entry point.
        vertex_entry: &'a str,
        /// The fragment shader.
        fragment: &'a [u32],
        /// Its entry point.
        fragment_entry: &'a str,
        /// The vertex attributes, by location.
        attrs: &'a [VertexAttr],
        /// Bytes from one vertex to the next (0 for no vertex buffer).
        stride: u32,
        /// The format of the images it draws into.
        target: Format,
        /// How it blends.
        blend: Blend,
    },
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

/// Create an image, lending it for a scope. Its contents start undefined
/// (zeros in [`SimGpu`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateImage {
    /// Its width, in pixels; at least 1.
    pub width: u32,
    /// Its height, in pixels; at least 1.
    pub height: u32,
    /// Its pixel format.
    pub format: Format,
    /// What it is for.
    pub usage: ImageUsage,
}

impl CreateImage {
    /// Describes a `width` by `height` image in `format`, for `usage`.
    pub const fn new(width: u32, height: u32, format: Format, usage: ImageUsage) -> Self {
        Self {
            width,
            height,
            format,
            usage,
        }
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
        }
    }

    /// A draw program.
    pub const fn draw(label: &'a str, kind: ProgramKind<'a>, bindings: &'a [BindingKind]) -> Self {
        Self {
            label,
            kind,
            bindings,
            push_constants: 0,
        }
    }

    /// Takes `bytes` of push constants.
    #[must_use]
    pub const fn push_constants(mut self, bytes: u32) -> Self {
        self.push_constants = bytes;
        self
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

/// Write the whole of `image`: `data` is its pixels, rows top to bottom,
/// tightly packed.
#[derive(Debug, Clone, Copy)]
pub struct UploadImage<'a> {
    /// Where.
    pub image: Image,
    /// What: `width * height * bytes_per_pixel` bytes.
    pub data: &'a [u8],
}

impl<'a> UploadImage<'a> {
    /// Writes `data` into `image`.
    pub const fn new(image: Image, data: &'a [u8]) -> Self {
        Self { image, data }
    }
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

/// Draw `vertex_count` vertices (`instance_count` times) with a draw
/// program into `target`, after clearing it to `clear` if given.
#[derive(Debug, Clone, Copy)]
pub struct Draw<'a> {
    /// The program.
    pub program: Program,
    /// The image drawn into: [`ImageUsage::TARGET`], in the program's
    /// target format.
    pub target: Image,
    /// Clear the target to this colour first.
    pub clear: Option<[f32; 4]>,
    /// The vertices, if the program reads any.
    pub vertices: Option<Buffer>,
    /// How many vertices.
    pub vertex_count: u32,
    /// How many instances.
    pub instance_count: u32,
    /// Its bindings, in order.
    pub bindings: &'a [Bind],
    /// Its push constants.
    pub push: &'a [u8],
}

impl<'a> Draw<'a> {
    /// Draws `vertex_count` vertices of `program` into `target`.
    pub const fn new(program: Program, target: Image, vertex_count: u32) -> Self {
        Self {
            program,
            target,
            clear: None,
            vertices: None,
            vertex_count,
            instance_count: 1,
            bindings: &[],
            push: &[],
        }
    }

    /// Clears the target first.
    #[must_use]
    pub const fn clear(mut self, rgba: [f32; 4]) -> Self {
        self.clear = Some(rgba);
        self
    }

    /// Reads vertices from `buffer`.
    #[must_use]
    pub const fn vertices(mut self, buffer: Buffer) -> Self {
        self.vertices = Some(buffer);
        self
    }

    /// Draws `count` instances.
    #[must_use]
    pub const fn instances(mut self, count: u32) -> Self {
        self.instance_count = count;
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

/// Read the whole of `image`, waiting for the work before it: rows top to
/// bottom, tightly packed.
#[derive(Debug)]
pub struct DownloadImage<'a> {
    /// From.
    pub image: Image,
    /// Into: `width * height * bytes_per_pixel` bytes.
    pub out: &'a mut [u8],
}

impl<'a> DownloadImage<'a> {
    /// Reads `image` into `out`.
    pub fn new(image: Image, out: &'a mut [u8]) -> Self {
        Self { image, out }
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
    Dispatch<'_> => Result<(), GpuError>;
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
        CreateProgram,
        Upload,
        UploadImage,
        CopyBuffer,
        Dispatch,
        Draw,
        Download,
        DownloadImage,
        Finish,
        DeviceInfo,
        Submit
    );
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
    CreateProgram<'_> => CreateProgram;
    Upload<'_> => Upload;
    UploadImage<'_> => UploadImage;
    CopyBuffer => CopyBuffer;
    Dispatch<'_> => Dispatch;
    Draw<'_> => Draw;
    Download<'_> => Download;
    DownloadImage<'_> => DownloadImage;
    Finish => Finish;
    DeviceInfo => DeviceInfo;
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
    Image {
        width: u32,
        height: u32,
        format: Format,
        usage: ImageUsage,
    },
    Program {
        label: String,
        compute: bool,
        bindings: Vec<BindingKind>,
        push_constants: u32,
        target: Option<Format>,
        stride: u32,
    },
}
