//! What a draw program is (its pipeline) and what a draw draws into.

use crate::{Buffer, Format, Image};

/// How many colour targets a draw writes at most.
pub const MAX_TARGETS: usize = 8;

/// How many vertex buffers a draw reads at most.
pub const MAX_VERTEX_BUFFERS: usize = 8;

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
    /// Two `f16`s.
    F16x2,
    /// Four `f16`s.
    F16x4,
    /// One `u32`.
    U32,
    /// Two `u32`s.
    U32x2,
    /// Three `u32`s.
    U32x3,
    /// Four `u32`s.
    U32x4,
    /// One `i32`.
    I32,
    /// Two `i32`s.
    I32x2,
    /// Three `i32`s.
    I32x3,
    /// Four `i32`s.
    I32x4,
    /// Four `u8`s, read as `f32`s in [0, 1]: colours.
    U8x4Norm,
    /// Four `i8`s, read as `f32`s in [-1, 1]: normals.
    I8x4Norm,
    /// Two `u16`s, read as `f32`s in [0, 1]: texture coordinates.
    U16x2Norm,
    /// Four `u16`s, read as `f32`s in [0, 1].
    U16x4Norm,
    /// Four `u8`s, as `u32`s: joint indices.
    U8x4,
}

impl AttrFormat {
    /// Its size, in bytes.
    pub const fn size(self) -> u32 {
        match self {
            Self::U8x4Norm | Self::I8x4Norm | Self::U16x2Norm | Self::U8x4 | Self::F16x2 => 4,
            Self::F32 | Self::U32 | Self::I32 => 4,
            Self::F16x4 | Self::U16x4Norm | Self::F32x2 | Self::U32x2 | Self::I32x2 => 8,
            Self::F32x3 | Self::U32x3 | Self::I32x3 => 12,
            Self::F32x4 | Self::U32x4 | Self::I32x4 => 16,
        }
    }
}

/// One vertex attribute: what the shader reads at `location`, `offset`
/// bytes into each vertex (or instance) of its buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VertexAttr {
    /// The shader's input location.
    pub location: u32,
    /// Where it starts in the vertex.
    pub offset: u32,
    /// What it is.
    pub format: AttrFormat,
}

impl VertexAttr {
    /// Describes the attribute at `location`.
    pub const fn new(location: u32, offset: u32, format: AttrFormat) -> Self {
        Self {
            location,
            offset,
            format,
        }
    }
}

/// Whether a vertex buffer advances per vertex or per instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Step {
    /// Each vertex reads the next element.
    #[default]
    Vertex,
    /// Each instance reads the next element: per-instance data.
    Instance,
}

/// One vertex buffer's layout: elements `stride` bytes apart, each holding
/// `attrs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VertexLayout<'a> {
    /// Bytes from one element to the next.
    pub stride: u32,
    /// What advances it.
    pub step: Step,
    /// What each element holds.
    pub attrs: &'a [VertexAttr],
}

impl<'a> VertexLayout<'a> {
    /// Per-vertex elements of `stride` bytes holding `attrs`.
    pub const fn new(stride: u32, attrs: &'a [VertexAttr]) -> Self {
        Self {
            stride,
            step: Step::Vertex,
            attrs,
        }
    }

    /// Per-instance elements instead.
    #[must_use]
    pub const fn per_instance(mut self) -> Self {
        self.step = Step::Instance;
        self
    }
}

/// A factor of a blend equation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendFactor {
    /// 0.
    Zero,
    /// 1.
    One,
    /// The shader's output.
    Src,
    /// 1 minus the shader's output.
    OneMinusSrc,
    /// What the target holds.
    Dst,
    /// 1 minus what the target holds.
    OneMinusDst,
    /// The shader's alpha.
    SrcAlpha,
    /// 1 minus the shader's alpha.
    OneMinusSrcAlpha,
    /// The target's alpha.
    DstAlpha,
    /// 1 minus the target's alpha.
    OneMinusDstAlpha,
}

/// How a blend equation combines its two terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendOp {
    /// `src * sf + dst * df`.
    Add,
    /// `src * sf - dst * df`.
    Subtract,
    /// `dst * df - src * sf`.
    ReverseSubtract,
    /// The smaller of `src` and `dst` (factors ignored).
    Min,
    /// The larger (factors ignored).
    Max,
}

/// One blend equation: `src * src_factor (op) dst * dst_factor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlendEquation {
    /// The shader output's factor.
    pub src: BlendFactor,
    /// The target's factor.
    pub dst: BlendFactor,
    /// How they combine.
    pub op: BlendOp,
}

impl BlendEquation {
    /// `src * src_factor (op) dst * dst_factor`.
    pub const fn new(src: BlendFactor, op: BlendOp, dst: BlendFactor) -> Self {
        Self { src, dst, op }
    }
}

/// How a draw combines with what a target holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Blend {
    /// Replace it.
    #[default]
    Replace,
    /// Premultiplied alpha: `src + dst * (1 - src.a)`, as compositors
    /// blend.
    PremultipliedAlpha,
    /// Straight alpha: `src * src.a + dst * (1 - src.a)`.
    Alpha,
    /// `src + dst`: light, particles.
    Additive,
    /// `src * dst`.
    Multiply,
    /// Any equations, for colour and for alpha.
    Custom {
        /// For red, green and blue.
        color: BlendEquation,
        /// For alpha.
        alpha: BlendEquation,
    },
}

impl Blend {
    /// Its equations, for colour and for alpha; `None` for
    /// [`Replace`](Self::Replace).
    pub const fn equations(self) -> Option<(BlendEquation, BlendEquation)> {
        use BlendFactor as F;
        let eq = BlendEquation::new;
        match self {
            Self::Replace => None,
            Self::PremultipliedAlpha => {
                let e = eq(F::One, BlendOp::Add, F::OneMinusSrcAlpha);
                Some((e, e))
            }
            Self::Alpha => Some((
                eq(F::SrcAlpha, BlendOp::Add, F::OneMinusSrcAlpha),
                eq(F::One, BlendOp::Add, F::OneMinusSrcAlpha),
            )),
            Self::Additive => {
                let e = eq(F::One, BlendOp::Add, F::One);
                Some((e, e))
            }
            Self::Multiply => {
                let e = eq(F::Dst, BlendOp::Add, F::Zero);
                Some((e, e))
            }
            Self::Custom { color, alpha } => Some((color, alpha)),
        }
    }
}

/// Which channels a draw writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorMask(pub(crate) u8);

impl ColorMask {
    /// Red.
    pub const R: Self = Self(1);
    /// Green.
    pub const G: Self = Self(2);
    /// Blue.
    pub const B: Self = Self(4);
    /// Alpha.
    pub const A: Self = Self(8);
    /// All four.
    pub const ALL: Self = Self(15);
    /// None: the target is only a place for depth to be tested against.
    pub const NONE: Self = Self(0);

    /// Whether every channel in `other` is written.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl Default for ColorMask {
    fn default() -> Self {
        Self::ALL
    }
}

impl core::ops::BitOr for ColorMask {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// One colour target of a draw program: its format, and how the program
/// writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorTarget {
    /// The format of the images drawn into.
    pub format: Format,
    /// How a draw combines with what they hold.
    pub blend: Blend,
    /// Which channels it writes.
    pub write: ColorMask,
}

impl ColorTarget {
    /// A target in `format`, replaced, every channel written.
    pub const fn new(format: Format) -> Self {
        Self {
            format,
            blend: Blend::Replace,
            write: ColorMask::ALL,
        }
    }

    /// Blended with `blend`.
    #[must_use]
    pub const fn blend(mut self, blend: Blend) -> Self {
        self.blend = blend;
        self
    }

    /// Writing only `mask`.
    #[must_use]
    pub const fn write(mut self, mask: ColorMask) -> Self {
        self.write = mask;
        self
    }
}

/// A comparison, for depth and stencil tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compare {
    /// Never passes.
    Never,
    /// Passes if the new value is less than the stored one.
    Less,
    /// If equal.
    Equal,
    /// If less or equal.
    LessOrEqual,
    /// If greater.
    Greater,
    /// If not equal.
    NotEqual,
    /// If greater or equal.
    GreaterOrEqual,
    /// Always passes: no test.
    Always,
}

/// What a stencil test does to the stored value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StencilOp {
    /// Leave it.
    Keep,
    /// Set it to 0.
    Zero,
    /// Set it to the draw's reference.
    Replace,
    /// Add 1, up to the maximum.
    IncrementClamp,
    /// Subtract 1, down to 0.
    DecrementClamp,
    /// Flip its bits.
    Invert,
    /// Add 1, wrapping.
    IncrementWrap,
    /// Subtract 1, wrapping.
    DecrementWrap,
}

/// The stencil test for one face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StencilFace {
    /// How the reference compares to the stored value, both masked by
    /// `read_mask`.
    pub compare: Compare,
    /// What happens when the stencil test fails.
    pub fail: StencilOp,
    /// When both tests pass.
    pub pass: StencilOp,
    /// When the stencil test passes and the depth test fails.
    pub depth_fail: StencilOp,
    /// The bits compared.
    pub read_mask: u32,
    /// The bits written.
    pub write_mask: u32,
}

impl StencilFace {
    /// Compares by `compare`, and on passing both tests does `pass`;
    /// otherwise keeps the value. All bits are read and written.
    pub const fn new(compare: Compare, pass: StencilOp) -> Self {
        Self {
            compare,
            fail: StencilOp::Keep,
            pass,
            depth_fail: StencilOp::Keep,
            read_mask: !0,
            write_mask: !0,
        }
    }
}

/// A draw program's depth (and stencil) test: the format of the depth
/// images it draws with, and what it does with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DepthState {
    /// The format: one of [`Format`]'s depth formats.
    pub format: Format,
    /// How a fragment's depth compares to the stored one;
    /// [`Compare::Always`] for no test.
    pub compare: Compare,
    /// Whether passing fragments write their depth.
    pub write: bool,
    /// The stencil test for front and back faces, if any (the format must
    /// have stencil).
    pub stencil: Option<(StencilFace, StencilFace)>,
}

impl DepthState {
    /// The usual: nearer fragments (less depth) win, and write their depth.
    pub const fn new(format: Format) -> Self {
        Self {
            format,
            compare: Compare::Less,
            write: true,
            stencil: None,
        }
    }

    /// Compares by `compare`.
    #[must_use]
    pub const fn compare(mut self, compare: Compare) -> Self {
        self.compare = compare;
        self
    }

    /// Writes depth if `write`.
    #[must_use]
    pub const fn write(mut self, write: bool) -> Self {
        self.write = write;
        self
    }

    /// With a stencil test, the same for both faces.
    #[must_use]
    pub const fn stencil(mut self, face: StencilFace) -> Self {
        self.stencil = Some((face, face));
        self
    }
}

/// What a draw assembles its vertices into.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Topology {
    /// Each vertex a point.
    PointList,
    /// Each pair a line.
    LineList,
    /// A line through every vertex.
    LineStrip,
    /// Each three a triangle.
    #[default]
    TriangleList,
    /// Each vertex, with the two before it, a triangle.
    TriangleStrip,
    /// Each vertex, with the one before it and the first, a triangle.
    TriangleFan,
}

/// Which triangles are not drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Cull {
    /// Draw both faces.
    #[default]
    None,
    /// Skip front faces.
    Front,
    /// Skip back faces.
    Back,
}

/// Which winding faces the viewer, in framebuffer coordinates (y down).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FrontFace {
    /// Counter-clockwise.
    #[default]
    CounterClockwise,
    /// Clockwise.
    Clockwise,
}

/// How triangles are filled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PolygonMode {
    /// Solid.
    #[default]
    Fill,
    /// Their edges: wireframe (where the device can).
    Line,
    /// Their corners (where the device can).
    Point,
}

/// How a draw program rasterizes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Raster {
    /// What vertices make.
    pub topology: Topology,
    /// Which faces are skipped.
    pub cull: Cull,
    /// Which winding is the front.
    pub front_face: FrontFace,
    /// How triangles are filled.
    pub polygon: PolygonMode,
    /// For an indexed strip or fan: whether the all-ones index (`0xFFFF`
    /// or `0xFFFFFFFF`) starts a new one.
    pub restart: bool,
}

/// A draw program's pipeline: its shaders, its vertex buffers, what it
/// draws into and how.
#[derive(Debug, Clone, Copy)]
pub struct DrawPipeline<'a> {
    /// The vertex shader.
    pub vertex: &'a [u32],
    /// Its entry point.
    pub vertex_entry: &'a str,
    /// The fragment shader.
    pub fragment: &'a [u32],
    /// Its entry point.
    pub fragment_entry: &'a str,
    /// The vertex buffers it reads, by slot.
    pub vertex_buffers: [Option<VertexLayout<'a>>; MAX_VERTEX_BUFFERS],
    /// The colour targets it writes, by slot.
    pub targets: [Option<ColorTarget>; MAX_TARGETS],
    /// Its depth and stencil test, if it has one.
    pub depth: Option<DepthState>,
    /// How it rasterizes.
    pub raster: Raster,
    /// Samples per pixel: 1, or 2, 4, 8, ... for multisampling. Every
    /// image it draws into has this many.
    pub samples: u32,
}

impl<'a> DrawPipeline<'a> {
    /// Draws triangles into one target in `target`, replacing it, with no
    /// vertex buffers, depth or multisampling.
    pub const fn new(
        vertex: &'a [u32],
        vertex_entry: &'a str,
        fragment: &'a [u32],
        fragment_entry: &'a str,
        target: Format,
    ) -> Self {
        let mut targets = [None; MAX_TARGETS];
        targets[0] = Some(ColorTarget::new(target));
        Self {
            vertex,
            vertex_entry,
            fragment,
            fragment_entry,
            vertex_buffers: [None; MAX_VERTEX_BUFFERS],
            targets,
            depth: None,
            raster: Raster {
                topology: Topology::TriangleList,
                cull: Cull::None,
                front_face: FrontFace::CounterClockwise,
                polygon: PolygonMode::Fill,
                restart: false,
            },
            samples: 1,
        }
    }

    /// Reads vertex buffer 0, per vertex: elements `stride` bytes apart.
    #[must_use]
    pub const fn vertices(self, stride: u32, attrs: &'a [VertexAttr]) -> Self {
        self.vertex_buffer(0, VertexLayout::new(stride, attrs))
    }

    /// Reads vertex buffer `slot` with `layout`.
    #[must_use]
    pub const fn vertex_buffer(mut self, slot: usize, layout: VertexLayout<'a>) -> Self {
        self.vertex_buffers[slot] = Some(layout);
        self
    }

    /// Blends target 0 with `blend`.
    #[must_use]
    pub const fn blend(mut self, blend: Blend) -> Self {
        if let Some(t) = &mut self.targets[0] {
            t.blend = blend;
        }
        self
    }

    /// Sets colour target `slot` (`None` for none).
    #[must_use]
    pub const fn target(mut self, slot: usize, target: Option<ColorTarget>) -> Self {
        self.targets[slot] = target;
        self
    }

    /// With a depth (and stencil) test.
    #[must_use]
    pub const fn depth(mut self, depth: DepthState) -> Self {
        self.depth = Some(depth);
        self
    }

    /// Rasterizes by `raster`.
    #[must_use]
    pub const fn raster(mut self, raster: Raster) -> Self {
        self.raster = raster;
        self
    }

    /// Assembles vertices into `topology`.
    #[must_use]
    pub const fn topology(mut self, topology: Topology) -> Self {
        self.raster.topology = topology;
        self
    }

    /// Skips `cull` faces.
    #[must_use]
    pub const fn cull(mut self, cull: Cull, front_face: FrontFace) -> Self {
        self.raster.cull = cull;
        self.raster.front_face = front_face;
        self
    }

    /// With `samples` samples per pixel.
    #[must_use]
    pub const fn samples(mut self, samples: u32) -> Self {
        self.samples = samples;
        self
    }
}

/// What a draw does first with what a target holds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Load {
    /// Keeps it: draws on top.
    #[default]
    Keep,
    /// Clears it to this colour (RGBA; integer formats take the values as
    /// integers).
    Clear([f32; 4]),
    /// Need not keep it: the draw covers every pixel it cares about. The
    /// contents start undefined (in [`SimGpu`](crate::SimGpu), as they
    /// were).
    Discard,
}

/// A colour target of a draw: an image (one mip level and one layer of
/// it), what happens to what it holds, and where a multisampled image is
/// resolved to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Attachment {
    /// The image: [`ImageUsage::TARGET`](crate::ImageUsage::TARGET), in the
    /// program's format for this slot, with the program's samples.
    pub image: Image,
    /// Its mip level.
    pub mip: u32,
    /// Its layer (a cube's face, an array's element, a 3D image's slice).
    pub layer: u32,
    /// What happens first.
    pub load: Load,
    /// For a multisampled image: a single-sampled one of the same format
    /// and size the samples are averaged into after the draw.
    pub resolve: Option<Image>,
}

impl Attachment {
    /// Draws into `image` (level 0, layer 0), on top of what it holds.
    pub const fn new(image: Image) -> Self {
        Self {
            image,
            mip: 0,
            layer: 0,
            load: Load::Keep,
            resolve: None,
        }
    }

    /// Clears it to `rgba` first.
    #[must_use]
    pub const fn clear(mut self, rgba: [f32; 4]) -> Self {
        self.load = Load::Clear(rgba);
        self
    }

    /// Does `load` first.
    #[must_use]
    pub const fn load(mut self, load: Load) -> Self {
        self.load = load;
        self
    }

    /// Draws into mip level `mip`, layer `layer`.
    #[must_use]
    pub const fn at(mut self, mip: u32, layer: u32) -> Self {
        self.mip = mip;
        self.layer = layer;
        self
    }

    /// Resolves into `image` after the draw.
    #[must_use]
    pub const fn resolve(mut self, image: Image) -> Self {
        self.resolve = Some(image);
        self
    }
}

/// What a draw does first with what a depth image holds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum DepthLoad {
    /// Keeps it.
    #[default]
    Keep,
    /// Clears depth and stencil to these.
    Clear {
        /// Depth, in [0, 1]: 1 is farthest.
        depth: f32,
        /// Stencil.
        stencil: u32,
    },
    /// Need not keep it.
    Discard,
}

/// The depth (and stencil) image of a draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DepthAttachment {
    /// The image: [`ImageUsage::TARGET`](crate::ImageUsage::TARGET), in the
    /// program's depth format, with its samples.
    pub image: Image,
    /// Its mip level.
    pub mip: u32,
    /// Its layer.
    pub layer: u32,
    /// What happens first.
    pub load: DepthLoad,
    /// Whether what the draw writes is kept after it (not, for depth used
    /// only within one draw).
    pub store: bool,
}

impl DepthAttachment {
    /// Tests against `image`, cleared to depth 1 (farthest) and stencil 0
    /// first, and keeps what the draw writes.
    pub const fn new(image: Image) -> Self {
        Self {
            image,
            mip: 0,
            layer: 0,
            load: DepthLoad::Clear {
                depth: 1.0,
                stencil: 0,
            },
            store: true,
        }
    }

    /// Does `load` first.
    #[must_use]
    pub const fn load(mut self, load: DepthLoad) -> Self {
        self.load = load;
        self
    }

    /// Keeps what the draw writes if `store`.
    #[must_use]
    pub const fn store(mut self, store: bool) -> Self {
        self.store = store;
        self
    }

    /// Uses mip level `mip`, layer `layer`.
    #[must_use]
    pub const fn at(mut self, mip: u32, layer: u32) -> Self {
        self.mip = mip;
        self.layer = layer;
        self
    }
}

/// A vertex buffer bound to a draw, from `offset` bytes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexBinding {
    /// The buffer: [`BufferUsage::VERTEX`](crate::BufferUsage::VERTEX).
    pub buffer: Buffer,
    /// Where its first element is.
    pub offset: u64,
}

/// The type of a draw's indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexFormat {
    /// `u16`s.
    U16,
    /// `u32`s.
    U32,
}

impl IndexFormat {
    /// An index's size, in bytes.
    pub const fn size(self) -> u64 {
        match self {
            Self::U16 => 2,
            Self::U32 => 4,
        }
    }
}

/// A draw's index buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexBuffer {
    /// The buffer: [`BufferUsage::INDEX`](crate::BufferUsage::INDEX).
    pub buffer: Buffer,
    /// Where the first index is (a multiple of the index size).
    pub offset: u64,
    /// Their type.
    pub format: IndexFormat,
}

/// Where a draw's normalized device coordinates land, in pixels of the
/// target: x and y from the top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height: negative flips it, for y up.
    pub height: f32,
    /// The depth that 0 maps to.
    pub min_depth: f32,
    /// The depth that 1 maps to.
    pub max_depth: f32,
}

impl Viewport {
    /// `width` by `height` pixels from `x`, `y`, depths 0 to 1.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            min_depth: 0.0,
            max_depth: 1.0,
        }
    }
}

/// A rectangle of pixels, from the top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Left.
    pub x: i32,
    /// Top.
    pub y: i32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl Rect {
    /// `width` by `height` pixels from `x`, `y`.
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}
