//! [`SimGpu`]: a GPU in memory, for tests.

use std::boxed::Box;
use std::collections::{BTreeMap, HashMap};
use std::string::String;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

use seam_core::{Closing, Inspect, Opening, Perform};

use crate::validate::{self, Lookup};
use crate::{
    AddressMode, Bind, BlitImage, Buffer, BufferAddress, BufferUsage, Constant, CopyBuffer,
    CopyImage, CreateBuffer, CreateImage, CreateProgram, CreateQueries, CreateSampler, DepthLoad,
    DeviceInfo, Dispatch, DispatchIndirect, Download, DownloadImage, Draw, DrawInfo, FillBuffer,
    Filter, Finish, Format, GenerateMips, GpuError, GpuFeatures, GpuInfo, GpuKind, Image,
    ImageRegion, Import, IndexFormat, Indirect, IsDone, Limits, Load, MAX_TARGETS,
    MAX_VERTEX_BUFFERS, Program, ProgramKind, Queries, QueryKind, ReadQueries, Rect, ResInfo,
    Sampler, Submit, Upload, UploadImage, Viewport, WriteTimestamp,
};

/// What [`SimGpu`] was asked to do, in order: for tests to assert on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// A buffer was created.
    CreateBuffer(Buffer),
    /// An image was created.
    CreateImage(Image),
    /// A sampler was created.
    CreateSampler(Sampler),
    /// Queries were created.
    CreateQueries(Queries),
    /// A timestamp was written into a query slot.
    WriteTimestamp(Queries, u32),
    /// An image was imported.
    Import(Image),
    /// A program was created, with its label.
    CreateProgram(Program, String),
    /// A resource's scope closed.
    Destroy(u64),
    /// `len` bytes were uploaded to a buffer at `offset`.
    Upload {
        /// Which buffer.
        buffer: Buffer,
        /// Where.
        offset: u64,
        /// How much.
        len: u64,
    },
    /// An image was uploaded.
    UploadImage(Image),
    /// Bytes were copied between buffers.
    Copy(CopyBuffer),
    /// Pixels were copied between images.
    CopyImage(CopyImage),
    /// Pixels were blitted between images.
    Blit(BlitImage),
    /// An image's mip levels were filled.
    GenerateMips(Image),
    /// A buffer's range was filled.
    Fill(FillBuffer),
    /// A compute program ran.
    Dispatch {
        /// Its label.
        label: String,
        /// Over how many workgroups.
        groups: [u32; 3],
    },
    /// A draw program drew.
    Draw {
        /// Its label.
        label: String,
        /// Into which image: its first target (or its depth image).
        target: Image,
        /// How many vertices.
        vertex_count: u32,
        /// How many instances.
        instance_count: u32,
    },
    /// A buffer was read back.
    Download {
        /// Which buffer.
        buffer: Buffer,
        /// From where.
        offset: u64,
        /// How much.
        len: u64,
    },
    /// An image was read back.
    DownloadImage(Image),
    /// Everything was waited for.
    Finish,
    /// The work so far was submitted.
    Submit,
}

/// A program's work, for a [`SimGpu`] emulator to do on the CPU: the
/// bound resources' bytes, by binding index, and what the program was run
/// with.
pub struct Run<'a> {
    label: &'a str,
    constants: Vec<Constant>,
    /// Each buffer with an address: where it starts, and its slot.
    addresses: Vec<(u64, usize)>,
    /// What an occlusion query of the draw counts.
    passed: u64,
    groups: [u32; 3],
    push: &'a [u8],
    /// For each binding, the slot holding its resource's bytes (`None`
    /// for a sampler, which has none).
    bound: Vec<Option<usize>>,
    /// For each binding, its sampler, if it is a sampled image or one.
    samplers: Vec<Option<CreateSampler>>,
    slots: Vec<Slot>,
    targets: [Option<Area>; MAX_TARGETS],
    depth: Option<Area>,
    vertex_buffers: [Option<(usize, u64)>; MAX_VERTEX_BUFFERS],
    index: Option<(usize, u64, IndexFormat)>,
    draw: Option<DrawArgs>,
}

/// One level and layer of an image in a slot: where its bytes are.
#[derive(Clone, Copy)]
struct Area {
    slot: usize,
    offset: usize,
    len: usize,
    width: u32,
    height: u32,
}

/// What a draw was asked to draw.
#[derive(Clone, Copy)]
struct DrawArgs {
    vertex_count: u32,
    instance_count: u32,
    first: u32,
    base_vertex: i32,
    first_instance: u32,
    viewport: Viewport,
    scissor: Rect,
}

struct Slot {
    id: u64,
    data: Vec<u8>,
    image: Option<CreateImage>,
}

/// Where level `mip`, layer (or slice) `layer` of an image is in its
/// bytes, which hold every level in turn, each every layer (or slice) in
/// turn, each rows top to bottom.
pub(crate) fn area_of(op: &CreateImage, mip: u32, layer: u32) -> (usize, usize, u32, u32) {
    let before: u64 = (0..mip).map(|m| op.level_bytes(m)).sum();
    let (w, h, _) = op.extent(mip);
    let one = op.format.size(w, h);
    (
        (before + u64::from(layer) * one) as usize,
        one as usize,
        w,
        h,
    )
}

/// Each run of bytes of `region` within its image's bytes, in the order
/// uploads and downloads lay them out: by layer, slice, then row of
/// blocks.
pub(crate) fn region_rows(op: &CreateImage, region: &ImageRegion) -> Vec<(usize, usize)> {
    let (layers, [w, h, d]) = region.resolve(op);
    let (lw, _, ld) = op.extent(region.mip);
    let (bw, bh, bytes) = op.format.block();
    let row_bytes = (lw.div_ceil(bw) * bytes) as usize;
    let run = (w.div_ceil(bw) * bytes) as usize;
    let [x, y, z] = region.offset;
    let mut rows = Vec::new();
    for layer in region.layer..region.layer + layers {
        for slice in z..z + d {
            // A volume's slices, and an array's layers, are both laid one
            // after another.
            let (start, ..) = area_of(op, region.mip, layer * ld + slice);
            for row in (y / bh)..(y + h).div_ceil(bh) {
                let at = start + row as usize * row_bytes + (x / bw * bytes) as usize;
                rows.push((at, run));
            }
        }
    }
    rows
}

/// Level `mip`, slice (or layer) `slice` of an image, decoded.
fn decode_slice(op: &CreateImage, data: &[u8], mip: u32, slice: u32) -> (Vec<[f32; 4]>, u32, u32) {
    let (at, len, w, h) = area_of(op, mip, slice);
    let n = op.format.bytes_per_pixel() as usize;
    let pixels = data[at..at + len]
        .chunks(n)
        .map(|px| decode_pixel(op.format, px))
        .collect();
    (pixels, w, h)
}

/// One pixel of `format`, from RGBA: depth in red, for a depth format.
fn encode_pixel(format: Format, rgba: [f32; 4]) -> Vec<u8> {
    if format.is_depth() {
        depth_pixel(format, rgba[0], 0)
    } else {
        clear_pixel(format, rgba)
    }
}

/// Bilinear (or nearest) reads of a decoded `w` by `h` slice, at
/// continuous coordinates in pixels, clamped to its edges.
fn read(pixels: &[[f32; 4]], w: u32, h: u32, x: f32, y: f32, filter: Filter) -> [f32; 4] {
    let at = |x: i64, y: i64| {
        let (x, y) = (x.clamp(0, i64::from(w) - 1), y.clamp(0, i64::from(h) - 1));
        pixels[(y * i64::from(w) + x) as usize]
    };
    match filter {
        Filter::Nearest => at(x.floor() as i64, y.floor() as i64),
        Filter::Linear => {
            let (x, y) = (x - 0.5, y - 0.5);
            let (x0, y0) = (x.floor(), y.floor());
            let (fx, fy) = (x - x0, y - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let [a, b, c, d] = [
                at(x0, y0),
                at(x0 + 1, y0),
                at(x0, y0 + 1),
                at(x0 + 1, y0 + 1),
            ];
            core::array::from_fn(|k| {
                let top = a[k] + (b[k] - a[k]) * fx;
                let bottom = c[k] + (d[k] - c[k]) * fx;
                top + (bottom - top) * fy
            })
        }
    }
}

/// An image's bytes: every level, every layer.
pub(crate) fn image_len(op: &CreateImage) -> usize {
    (0..op.mips).map(|m| op.level_bytes(m)).sum::<u64>() as usize
}

impl Run<'_> {
    /// The program's label.
    pub fn label(&self) -> &str {
        self.label
    }

    /// The bytes of a buffer with an address (made with
    /// [`BufferUsage::ADDRESS`]) from `address` on: what a shader reaches
    /// through it.
    ///
    /// # Panics
    ///
    /// If `address` is in no such buffer.
    pub fn memory(&mut self, address: u64) -> &mut [u8] {
        let (base, slot) = self
            .addresses
            .iter()
            .copied()
            .find(|&(base, slot)| {
                address >= base && address - base < self.slots[slot].data.len() as u64
            })
            .expect("seam-ash: the address is in no buffer");
        &mut self.slots[slot].data[(address - base) as usize..]
    }

    /// For a draw with an occlusion query: how many samples passed, for
    /// the query to count (0 unless this is called).
    pub fn pass_samples(&mut self, samples: u64) {
        self.passed = samples;
    }

    /// Specialization constant `id`'s bits, if the program set it.
    pub fn constant(&self, id: u32) -> Option<u32> {
        self.constants.iter().find(|c| c.id == id).map(|c| c.bits)
    }

    /// The workgroups a dispatch asked for (`[0, 0, 0]` for a draw).
    pub fn groups(&self) -> [u32; 3] {
        self.groups
    }

    /// The push constants.
    pub fn push(&self) -> &[u8] {
        self.push
    }

    /// The bytes of the buffer or image at binding `i`. A resource bound
    /// twice is the same bytes.
    ///
    /// # Panics
    ///
    /// If there is no binding `i`.
    pub fn buffer(&mut self, i: usize) -> &mut [u8] {
        let slot = self.slot(i);
        &mut self.slots[slot].data
    }

    fn slot(&self, i: usize) -> usize {
        self.bound[i].expect("seam-ash: a sampler binding has no bytes")
    }

    /// The width and height of the image at binding `i`, or `None` for a
    /// buffer.
    pub fn image_size(&self, i: usize) -> Option<(u32, u32)> {
        self.bound[i]
            .and_then(|s| self.slots[s].image)
            .map(|op| (op.width, op.height))
    }

    /// The description of the image at binding `i`, or `None` for a
    /// buffer: its shape, levels, layers and format. Its bytes
    /// ([`buffer`](Self::buffer)) hold every level in turn, largest first,
    /// each every layer (or slice) in turn.
    pub fn image_info(&self, i: usize) -> Option<CreateImage> {
        self.bound[i].and_then(|s| self.slots[s].image)
    }

    /// The sampler at binding `i`: bound with a sampled image, or alone.
    pub fn sampler(&self, i: usize) -> Option<CreateSampler> {
        self.samplers[i]
    }

    /// The texel at `x`, `y` of the image at binding `i`, as linear RGBA:
    /// what a shader's `texelFetch` reads (unorm in [0, 1], sRGB decoded,
    /// one-channel formats as `[r, 0, 0, 1]`).
    ///
    /// # Panics
    ///
    /// If binding `i` is not an image, or `x`, `y` is outside it.
    pub fn texel(&self, i: usize, x: u32, y: u32) -> [f32; 4] {
        self.texel_in(i, 0, 0, x, y)
    }

    /// The texel at `x`, `y` of mip level `mip`, layer (or slice) `layer`
    /// of the image at binding `i`, as [`texel`](Self::texel) reads it.
    ///
    /// # Panics
    ///
    /// If binding `i` is not an image, the level, layer or texel is
    /// outside it, or its format is block-compressed (the simulation does
    /// not decode those).
    pub fn texel_in(&self, i: usize, mip: u32, layer: u32, x: u32, y: u32) -> [f32; 4] {
        let slot = &self.slots[self.slot(i)];
        let op = slot.image.expect("seam-ash: the binding is not an image");
        assert!(
            !op.format.is_compressed(),
            "seam-ash: SimGpu does not decode {:?}",
            op.format
        );
        let (at, _, w, h) = area_of(&op, mip, layer);
        assert!(
            x < w && y < h,
            "seam-ash: texel {x}, {y} is outside a {w}x{h} level"
        );
        let n = op.format.bytes_per_pixel() as usize;
        let at = at + (y as usize * w as usize + x as usize) * n;
        decode_pixel(op.format, &slot.data[at..at + n])
    }

    /// Samples the image at binding `i` at `uv` (in [0, 1] across the
    /// image) with its sampler's filter and address modes: what a shader's
    /// `textureLod(.., uv, 0.0)` reads. Images have one level, so this is
    /// `texture` in a fragment shader too, up to the GPU's precision.
    ///
    /// # Panics
    ///
    /// If binding `i` is not a sampled image.
    pub fn sample(&self, i: usize, uv: [f32; 2]) -> [f32; 4] {
        self.sample_with(i, i, uv)
    }

    /// Samples the image (a texture) at binding `texture` with the sampler
    /// at binding `sampler`, as [`sample`](Self::sample) does: for shaders
    /// that bind them apart (WGSL, HLSL).
    ///
    /// # Panics
    ///
    /// If `texture` is not an image, or `sampler` not a sampler.
    pub fn sample_with(&self, texture: usize, sampler: usize, uv: [f32; 2]) -> [f32; 4] {
        let i = texture;
        let sampler = self.samplers[sampler].expect("seam-ash: the binding has no sampler");
        let (w, h) = self
            .image_size(i)
            .expect("seam-ash: the binding is not an image");
        let border = sampler.border.rgba();
        let fetch = |x: i64, y: i64| -> [f32; 4] {
            match (wrap(x, w, sampler.address_u), wrap(y, h, sampler.address_v)) {
                (Some(x), Some(y)) => self.texel(i, x, y),
                _ => border,
            }
        };
        let (u, v) = (uv[0] * w as f32, uv[1] * h as f32);
        match sampler.mag {
            Filter::Nearest => fetch(u.floor() as i64, v.floor() as i64),
            Filter::Linear => {
                let (u, v) = (u - 0.5, v - 0.5);
                let (x, y) = (u.floor(), v.floor());
                let (fx, fy) = (u - x, v - y);
                let (x, y) = (x as i64, y as i64);
                let [a, b, c, d] = [
                    fetch(x, y),
                    fetch(x + 1, y),
                    fetch(x, y + 1),
                    fetch(x + 1, y + 1),
                ];
                core::array::from_fn(|k| {
                    let top = a[k] + (b[k] - a[k]) * fx;
                    let bottom = c[k] + (d[k] - c[k]) * fx;
                    top + (bottom - top) * fy
                })
            }
        }
    }

    /// For a draw: target 0's pixels (already loaded: cleared, if the
    /// draw clears), and its width and height.
    ///
    /// # Panics
    ///
    /// If this is a dispatch, or the draw has no target 0.
    pub fn target(&mut self) -> (&mut [u8], u32, u32) {
        self.target_at(0)
            .expect("seam-ash: the draw has no target 0")
    }

    /// For a draw: target `slot`'s pixels (its level and layer), and its
    /// width and height; `None` if it has none. A multisampled target is
    /// one sample per pixel here, as if already resolved; the resolve
    /// image gets these pixels after the emulator returns.
    pub fn target_at(&mut self, slot: usize) -> Option<(&mut [u8], u32, u32)> {
        let a = self.targets.get(slot).copied().flatten()?;
        let data = &mut self.slots[a.slot].data[a.offset..a.offset + a.len];
        Some((data, a.width, a.height))
    }

    /// For a draw: the depth image's bytes (its level and layer), and its
    /// width and height, if it tests depth. Per pixel: `D16Unorm` a `u16`,
    /// `D32Float` an `f32`, `D24UnormS8Uint` a `u32` with depth in the low
    /// 24 bits and stencil in the high 8, `D32FloatS8Uint` an `f32` then a
    /// `u8`; little-endian.
    pub fn depth(&mut self) -> Option<(&mut [u8], u32, u32)> {
        let a = self.depth?;
        let data = &mut self.slots[a.slot].data[a.offset..a.offset + a.len];
        Some((data, a.width, a.height))
    }

    /// For a draw: vertex buffer 0's bytes, from its offset, if any.
    pub fn vertices(&self) -> Option<&[u8]> {
        self.vertex_buffer(0)
    }

    /// For a draw: vertex buffer `slot`'s bytes, from its offset, if any.
    pub fn vertex_buffer(&self, slot: usize) -> Option<&[u8]> {
        let (s, offset) = self.vertex_buffers.get(slot).copied().flatten()?;
        Some(&self.slots[s].data[offset as usize..])
    }

    /// For an indexed draw: the vertices it draws, by index: each index of
    /// the range drawn, plus the base vertex. `None` for a draw without
    /// indices.
    pub fn indices(&self) -> Option<Vec<u32>> {
        let (s, offset, format) = self.index?;
        let args = self.draw?;
        let bytes = &self.slots[s].data[offset as usize..];
        let size = format.size() as usize;
        let first = args.first as usize;
        Some(
            (first..first + args.vertex_count as usize)
                .map(|i| {
                    let b = &bytes[i * size..(i + 1) * size];
                    let index = match format {
                        IndexFormat::U16 => u32::from(u16::from_le_bytes([b[0], b[1]])),
                        IndexFormat::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                    };
                    index.wrapping_add_signed(args.base_vertex)
                })
                .collect(),
        )
    }

    /// For a draw: how many vertices (or indices) and instances.
    pub fn counts(&self) -> (u32, u32) {
        self.draw
            .map_or((0, 0), |d| (d.vertex_count, d.instance_count))
    }

    /// For a draw: its first vertex (or index), and its first instance.
    pub fn firsts(&self) -> (u32, u32) {
        self.draw.map_or((0, 0), |d| (d.first, d.first_instance))
    }

    /// For a draw: where it lands in its targets (the whole of them, if it
    /// gave no viewport).
    pub fn viewport(&self) -> Option<Viewport> {
        self.draw.map(|d| d.viewport)
    }

    /// For a draw: what it may write (the whole of its targets, if it gave
    /// no scissor).
    pub fn scissor(&self) -> Option<Rect> {
        self.draw.map(|d| d.scissor)
    }
}

/// A texel coordinate wrapped into `0..size` by `mode`, or `None` for the
/// border.
fn wrap(i: i64, size: u32, mode: AddressMode) -> Option<u32> {
    let n = i64::from(size);
    let i = match mode {
        AddressMode::ClampToEdge => i.clamp(0, n - 1),
        AddressMode::Repeat => i.rem_euclid(n),
        AddressMode::MirroredRepeat => {
            let m = i.rem_euclid(2 * n);
            if m < n { m } else { 2 * n - 1 - m }
        }
        AddressMode::ClampToBorder if (0..n).contains(&i) => i,
        AddressMode::ClampToBorder => return None,
    };
    Some(i as u32)
}

/// One pixel of `format`, as linear RGBA.
fn decode_pixel(format: Format, px: &[u8]) -> [f32; 4] {
    let unorm = |b: u8| f32::from(b) / 255.0;
    let snorm = |b: u8| (f32::from(b as i8) / 127.0).max(-1.0);
    let srgb = |b: u8| {
        let c = unorm(b);
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let u16_at = |i: usize| u16::from_le_bytes([px[i], px[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([px[i], px[i + 1], px[i + 2], px[i + 3]]);
    let unorm16 = |i: usize| f32::from(u16_at(i)) / 65535.0;
    let half = |i: usize| f16_value(u16_at(i));
    let float = |i: usize| f32::from_bits(u32_at(i));
    match format {
        Format::R8Unorm => [unorm(px[0]), 0.0, 0.0, 1.0],
        Format::Rg8Unorm => [unorm(px[0]), unorm(px[1]), 0.0, 1.0],
        Format::Rgba8Unorm => [unorm(px[0]), unorm(px[1]), unorm(px[2]), unorm(px[3])],
        Format::Rgba8Snorm => [snorm(px[0]), snorm(px[1]), snorm(px[2]), snorm(px[3])],
        Format::Bgra8Unorm => [unorm(px[2]), unorm(px[1]), unorm(px[0]), unorm(px[3])],
        Format::Rgba8Srgb => [srgb(px[0]), srgb(px[1]), srgb(px[2]), unorm(px[3])],
        Format::Bgra8Srgb => [srgb(px[2]), srgb(px[1]), srgb(px[0]), unorm(px[3])],
        Format::R8Uint => [f32::from(px[0]), 0.0, 0.0, 1.0],
        Format::Rgba8Uint => px[..4]
            .iter()
            .map(|b| f32::from(*b))
            .collect::<Vec<_>>()
            .try_into()
            .expect("four"),
        Format::R16Unorm => [unorm16(0), 0.0, 0.0, 1.0],
        Format::Rgba16Unorm => [unorm16(0), unorm16(2), unorm16(4), unorm16(6)],
        Format::R16Float => [half(0), 0.0, 0.0, 1.0],
        Format::Rg16Float => [half(0), half(2), 0.0, 1.0],
        Format::Rgba16Float => [half(0), half(2), half(4), half(6)],
        Format::R16Uint => [f32::from(u16_at(0)), 0.0, 0.0, 1.0],
        Format::R32Float => [float(0), 0.0, 0.0, 1.0],
        Format::Rg32Float => [float(0), float(4), 0.0, 1.0],
        Format::Rgba32Float => [float(0), float(4), float(8), float(12)],
        Format::R32Uint => [u32_at(0) as f32, 0.0, 0.0, 1.0],
        Format::Rg32Uint => [u32_at(0) as f32, u32_at(4) as f32, 0.0, 1.0],
        Format::Rgba32Uint => [
            u32_at(0) as f32,
            u32_at(4) as f32,
            u32_at(8) as f32,
            u32_at(12) as f32,
        ],
        Format::R32Sint => [u32_at(0) as i32 as f32, 0.0, 0.0, 1.0],
        Format::Rgb10A2Unorm => {
            let v = u32_at(0);
            let ten = |shift: u32| ((v >> shift) & 0x3ff) as f32 / 1023.0;
            [ten(0), ten(10), ten(20), (v >> 30) as f32 / 3.0]
        }
        Format::Rg11B10Float => {
            let v = u32_at(0);
            let small = |shift: u32, bits: u32| {
                f16_value((((v >> shift) & ((1 << bits) - 1)) << (15 - bits)) as u16)
            };
            [small(0, 11), small(11, 11), small(22, 10), 1.0]
        }
        Format::D16Unorm => [unorm16(0), 0.0, 0.0, 1.0],
        Format::D32Float | Format::D32FloatS8Uint => [float(0), 0.0, 0.0, 1.0],
        Format::D24UnormS8Uint => [(u32_at(0) & 0xff_ffff) as f32 / 16_777_215.0, 0.0, 0.0, 1.0],
        _ => unreachable!("compressed formats are not decoded"),
    }
}

/// IEEE half-precision bits as an `f32`.
fn f16_value(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let mant = f32::from(h & 0x3ff);
    sign * match exp {
        0 => mant * 2f32.powi(-24),
        0x1f if mant == 0.0 => f32::INFINITY,
        0x1f => f32::NAN,
        e => (1.0 + mant / 1024.0) * 2f32.powi(e - 15),
    }
}

type Emulator = Box<dyn FnMut(&mut Run<'_>) + Send>;

struct Entry {
    info: ResInfo,
    data: Vec<u8>,
}

#[derive(Default)]
struct State {
    next: u64,
    res: BTreeMap<u64, Entry>,
    emulators: HashMap<String, Emulator>,
    log: Vec<Recorded>,
}

impl Lookup for State {
    fn info(&self, id: u64) -> Option<&ResInfo> {
        self.res.get(&id).map(|e| &e.info)
    }
}

/// A simulated GPU: buffers and images are bytes in memory, uploads,
/// copies and downloads move them, and every operation is checked as
/// [`Ash`] checks it. A program runs by the CPU function given for its
/// label with [`emulate`](Self::emulate); one without does nothing, but is
/// recorded. A draw that clears clears its target either way. Nothing
/// waits.
///
/// Every clone is a handle to the same GPU.
///
#[cfg_attr(feature = "ash", doc = "[`Ash`]: crate::Ash")]
#[cfg_attr(
    not(feature = "ash"),
    doc = "[`Ash`]: https://docs.rs/seam-rs-ash/latest/seam_ash/struct.Ash.html"
)]
#[derive(Clone, Default)]
pub struct SimGpu(Arc<Mutex<State>>);

impl core::fmt::Debug for SimGpu {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let state = self.lock();
        f.debug_struct("SimGpu")
            .field("resources", &state.res.len())
            .field("emulated", &state.emulators.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SimGpu {
    /// An empty GPU.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs programs labelled `label` with `f`, on the CPU.
    pub fn emulate(
        &mut self,
        label: &str,
        f: impl FnMut(&mut Run<'_>) + Send + 'static,
    ) -> &mut Self {
        self.lock().emulators.insert(label.into(), Box::new(f));
        self
    }

    /// A buffer's bytes, while its scope is open.
    pub fn buffer(&self, buffer: Buffer) -> Option<Vec<u8>> {
        let state = self.lock();
        match state.res.get(&buffer.raw()) {
            Some(Entry {
                info: ResInfo::Buffer { .. },
                data,
            }) => Some(data.clone()),
            _ => None,
        }
    }

    /// An image's pixels, while its scope is open.
    pub fn image(&self, image: Image) -> Option<Vec<u8>> {
        let state = self.lock();
        match state.res.get(&image.raw()) {
            Some(Entry {
                info: ResInfo::Image { .. },
                data,
            }) => Some(data.clone()),
            _ => None,
        }
    }

    /// Everything performed, in order.
    pub fn log(&self) -> Vec<Recorded> {
        self.lock().log.clone()
    }

    /// How many resources are alive: for checking that scopes closed.
    pub fn live(&self) -> usize {
        self.lock().res.len()
    }
}

impl State {
    fn add(&mut self, info: ResInfo, len: usize) -> u64 {
        self.next += 1;
        let id = self.next;
        self.res.insert(
            id,
            Entry {
                info,
                data: std::vec![0; len],
            },
        );
        id
    }

    fn data(&mut self, id: u64) -> &mut Vec<u8> {
        &mut self.res.get_mut(&id).expect("checked to exist").data
    }

    /// Runs a program: takes the bound resources' bytes out, hands them to
    /// the emulator, puts them back.
    /// Runs the program's emulator, if it has one: what it says passed an
    /// occlusion test.
    fn run(&mut self, label: String, run_setup: RunSetup<'_>) -> u64 {
        let Some(mut emulator) = self.emulators.remove(&label) else {
            return 0;
        };
        let mut slots: Vec<Slot> = Vec::new();
        let mut slot_of = |state: &mut State, id: u64| -> usize {
            if let Some(i) = slots.iter().position(|s| s.id == id) {
                return i;
            }
            let entry = state.res.get_mut(&id).expect("checked to exist");
            let image = match entry.info {
                ResInfo::Image(op) => Some(op),
                _ => None,
            };
            slots.push(Slot {
                id,
                data: core::mem::take(&mut entry.data),
                image,
            });
            slots.len() - 1
        };
        let bound: Vec<Option<usize>> = run_setup
            .bindings
            .iter()
            .map(|b| {
                let id = match *b {
                    Bind::Uniform(x) | Bind::Storage(x) => x.raw(),
                    Bind::Sampled(x, _) | Bind::Texture(x) | Bind::StorageImage(x) => x.raw(),
                    Bind::Sampler(_) => return None,
                };
                Some(slot_of(self, id))
            })
            .collect();
        let samplers = run_setup
            .bindings
            .iter()
            .map(|b| match *b {
                Bind::Sampled(_, s) | Bind::Sampler(s) => validate::sampler(&*self, s.raw()).ok(),
                _ => None,
            })
            .collect();
        let mut targets = [None; MAX_TARGETS];
        let mut depth = None;
        let mut vertex_buffers = [None; MAX_VERTEX_BUFFERS];
        let mut index = None;
        let mut draw = None;
        if let Some(d) = run_setup.draw {
            let mut area = |state: &mut State, id: u64, mip: u32, layer: u32| -> Area {
                let op = image_of(state, id);
                let slot = slot_of(state, id);
                let (offset, len, width, height) = area_of(&op, mip, layer);
                Area {
                    slot,
                    offset,
                    len,
                    width,
                    height,
                }
            };
            for (slot, t) in d.targets.iter().enumerate() {
                if let Some(t) = t {
                    targets[slot] = Some(area(self, t.image.raw(), t.mip, t.layer));
                }
            }
            depth = d.depth.map(|t| area(self, t.image.raw(), t.mip, t.layer));
            for (slot, v) in d.vertex_buffers.iter().enumerate() {
                if let Some(v) = v {
                    vertex_buffers[slot] = Some((slot_of(self, v.buffer.raw()), v.offset));
                }
            }
            index = d
                .index
                .map(|i| (slot_of(self, i.buffer.raw()), i.offset, i.format));
            let size = targets
                .iter()
                .flatten()
                .chain(depth.iter())
                .next()
                .map_or((0, 0), |a| (a.width, a.height));
            draw = Some(DrawArgs {
                vertex_count: d.vertex_count,
                instance_count: d.instance_count,
                first: d.first,
                base_vertex: d.base_vertex,
                first_instance: d.first_instance,
                viewport: d.viewport.unwrap_or(Viewport::new(
                    0.0,
                    0.0,
                    size.0 as f32,
                    size.1 as f32,
                )),
                scissor: d.scissor.unwrap_or(Rect::new(0, 0, size.0, size.1)),
            });
        }
        let address_ids: Vec<u64> = self
            .res
            .iter()
            .filter(|(_, e)| {
                matches!(e.info, ResInfo::Buffer { usage, .. } if usage.contains(BufferUsage::ADDRESS))
            })
            .map(|(&id, _)| id)
            .collect();
        let addresses = address_ids
            .into_iter()
            .map(|id| (id << 32, slot_of(self, id)))
            .collect();
        let mut run = Run {
            label: &label,
            constants: run_setup.constants,
            addresses,
            passed: 0,
            groups: run_setup.groups,
            push: run_setup.push,
            bound,
            samplers,
            slots,
            targets,
            depth,
            vertex_buffers,
            index,
            draw,
        };
        emulator(&mut run);
        let passed = run.passed;
        for slot in run.slots {
            *self.data(slot.id) = slot.data;
        }
        self.emulators.insert(label, emulator);
        passed
    }
}

/// An image's description, whether its bytes are with it or lent to a
/// run.
fn image_of(state: &State, id: u64) -> CreateImage {
    match state.res[&id].info {
        ResInfo::Image(op) => op,
        _ => unreachable!("validated as an image"),
    }
}

struct RunSetup<'a> {
    constants: Vec<Constant>,
    bindings: &'a [Bind],
    push: &'a [u8],
    groups: [u32; 3],
    draw: Option<&'a Draw<'a>>,
}

impl Perform<Opening<CreateBuffer>> for SimGpu {
    fn perform(&mut self, Opening(op, _): Opening<CreateBuffer>) -> Result<Buffer, GpuError> {
        validate::create_buffer(&op)?;
        let len = usize::try_from(op.size).map_err(|_| GpuError::OutOfMemory)?;
        let mut state = self.lock();
        let id = state.add(
            ResInfo::Buffer {
                size: op.size,
                usage: op.usage,
            },
            len,
        );
        state.log.push(Recorded::CreateBuffer(Buffer(id)));
        Ok(Buffer(id))
    }
}

impl Perform<Opening<CreateImage>> for SimGpu {
    fn perform(&mut self, Opening(op, _): Opening<CreateImage>) -> Result<Image, GpuError> {
        validate::create_image(&op)?;
        let mut state = self.lock();
        let id = state.add(ResInfo::Image(op), image_len(&op));
        state.log.push(Recorded::CreateImage(Image(id)));
        Ok(Image(id))
    }
}

impl Perform<Opening<CreateSampler>> for SimGpu {
    fn perform(&mut self, Opening(op, _): Opening<CreateSampler>) -> Result<Sampler, GpuError> {
        let mut state = self.lock();
        let id = state.add(ResInfo::Sampler(op), 0);
        state.log.push(Recorded::CreateSampler(Sampler(id)));
        Ok(Sampler(id))
    }
}

impl Perform<Opening<CreateQueries>> for SimGpu {
    fn perform(&mut self, Opening(op, _): Opening<CreateQueries>) -> Result<Queries, GpuError> {
        validate::create_queries(&op)?;
        let mut state = self.lock();
        let info = ResInfo::Queries {
            kind: op.kind,
            written: std::vec![false; op.count as usize],
        };
        let id = state.add(info, op.count as usize * 8);
        state.log.push(Recorded::CreateQueries(Queries(id)));
        Ok(Queries(id))
    }
}

impl State {
    /// Writes `value` into slot `index` of queries `id`.
    fn record_query(&mut self, id: u64, index: u32, value: u64) {
        let entry = self.res.get_mut(&id).expect("validated");
        if let ResInfo::Queries { written, .. } = &mut entry.info {
            written[index as usize] = true;
        }
        let at = index as usize * 8;
        entry.data[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
}

/// A timestamp is the number of operations recorded before it, as
/// nanoseconds: in order, and the same every run.
impl Perform<WriteTimestamp> for SimGpu {
    fn perform(&mut self, op: WriteTimestamp) -> Result<(), GpuError> {
        let mut state = self.lock();
        validate::query_slot(
            &*state,
            "timestamp",
            (op.queries.raw(), op.index),
            QueryKind::Timestamp,
        )?;
        let now = state.log.len() as u64;
        state.record_query(op.queries.raw(), op.index, now);
        state
            .log
            .push(Recorded::WriteTimestamp(op.queries, op.index));
        Ok(())
    }
}

impl Perform<ReadQueries<'_>> for SimGpu {
    fn perform(&mut self, op: ReadQueries<'_>) -> Result<(), GpuError> {
        let mut state = self.lock();
        validate::read_queries(&*state, &op)?;
        let at = op.first as usize * 8;
        let data = state.data(op.queries.raw());
        for (i, out) in op.out.iter_mut().enumerate() {
            let b = at + i * 8;
            *out = u64::from_le_bytes(data[b..b + 8].try_into().expect("eight bytes"));
        }
        Ok(())
    }
}

/// A buffer's address is its ID, shifted up: what [`Run::memory`] finds it
/// by.
impl Perform<BufferAddress> for SimGpu {
    fn perform(&mut self, op: BufferAddress) -> Result<u64, GpuError> {
        let state = self.lock();
        validate::address(&*state, &op)?;
        Ok(op.0.raw() << 32)
    }
}

impl Perform<Opening<CreateProgram<'_>>> for SimGpu {
    fn perform(&mut self, Opening(op, _): Opening<CreateProgram<'_>>) -> Result<Program, GpuError> {
        validate::create_program(&op)?;
        let (compute, draw) = match &op.kind {
            ProgramKind::Compute { .. } => (true, None),
            ProgramKind::Draw(p) => (false, Some(DrawInfo::of(p))),
        };
        let mut state = self.lock();
        let (bindings, slots) = validate::slots(&op);
        let info = ResInfo::Program {
            label: op.label.into(),
            compute,
            bindings,
            slots,
            push_constants: op.push_constants,
            constants: op.constants.to_vec(),
            draw,
        };
        let id = state.add(info, 0);
        state
            .log
            .push(Recorded::CreateProgram(Program(id), op.label.into()));
        Ok(Program(id))
    }
}

macro_rules! closing {
    ($($acq:ty => $res:ty;)*) => {$(
        impl Perform<Closing<$acq>> for SimGpu {
            fn perform(&mut self, Closing(res, _): Closing<$acq>) -> Result<(), GpuError> {
                let mut state = self.lock();
                state.res.remove(&res.raw());
                state.log.push(Recorded::Destroy(res.raw()));
                Ok(())
            }
        }
    )*};
}

closing! {
    CreateQueries => Queries;
    CreateBuffer => Buffer;
    CreateImage => Image;
    CreateSampler => Sampler;
    CreateProgram<'_> => Program;
}

impl<B> Perform<Closing<Import<B>>> for SimGpu {
    fn perform(&mut self, Closing(image, _): Closing<Import<B>>) -> Result<(), GpuError> {
        let mut state = self.lock();
        state.res.remove(&image.raw());
        state.log.push(Recorded::Destroy(image.raw()));
        Ok(())
    }
}

/// Dmabufs, read through their descriptors: so a test's fake dmabufs are
/// memfds (or files) holding linear pixels. Real dmabufs cannot be read so,
/// and fail to import.
#[cfg(drm)]
mod dmabuf {
    use std::os::unix::fs::FileExt;

    use super::*;
    use crate::drm::{Dmabuf, DmabufFormat, DmabufFormats, Fourcc, Modifier};
    use crate::invalid;

    impl<'a> Perform<Opening<Import<&'a Dmabuf>>> for SimGpu {
        fn perform(
            &mut self,
            Opening(op, _): Opening<Import<&'a Dmabuf>>,
        ) -> Result<Image, GpuError> {
            let buf = op.source;
            validate::dmabuf(buf, op.usage)?;
            if buf.modifier != Modifier::LINEAR {
                return Err(GpuError::Unsupported(format!(
                    "{:?} dmabufs with modifier {:#x} on the simulated GPU; see DmabufFormats",
                    buf.format, buf.modifier.0
                )));
            }
            let (format, opaque) = buf.format.format().expect("validated");
            let plane = &buf.planes[0];
            let file = plane
                .fd
                .try_clone()
                .map(std::fs::File::from)
                .map_err(|e| invalid(format!("the dmabuf's descriptor: {e}")))?;
            let bpp = format.bytes_per_pixel() as usize;
            let row = buf.width as usize * bpp;
            let mut pixels = std::vec![0; row * buf.height as usize];
            for (y, out) in pixels.chunks_exact_mut(row).enumerate() {
                let at = u64::from(plane.offset) + y as u64 * u64::from(plane.stride);
                file.read_exact_at(out, at)
                    .map_err(|e| invalid(format!("reading the dmabuf's row {y}: {e}")))?;
            }
            if opaque {
                // The unused alpha reads as 1, as on a GPU.
                let one: &[u8] = if bpp == 8 { &[0x00, 0x3c] } else { &[0xff] };
                for px in pixels.chunks_exact_mut(bpp) {
                    px[bpp - one.len()..].copy_from_slice(one);
                }
            }
            let mut state = self.lock();
            let info = ResInfo::Image(CreateImage::new(buf.width, buf.height, format, op.usage));
            let id = state.add(info, 0);
            *state.data(id) = pixels;
            state.log.push(Recorded::Import(Image(id)));
            Ok(Image(id))
        }
    }

    impl Inspect<DmabufFormats> for SimGpu {
        fn inspect(&self, _: DmabufFormats) -> Result<Vec<DmabufFormat>, GpuError> {
            Ok(Fourcc::SUPPORTED
                .into_iter()
                .map(|fourcc| DmabufFormat {
                    fourcc,
                    modifier: Modifier::LINEAR,
                })
                .collect())
        }
    }
}

impl Perform<Upload<'_>> for SimGpu {
    fn perform(&mut self, op: Upload<'_>) -> Result<(), GpuError> {
        let mut state = self.lock();
        validate::upload(&*state, op.buffer.raw(), op.offset, op.data.len() as u64)?;
        let start = op.offset as usize;
        state.data(op.buffer.raw())[start..start + op.data.len()].copy_from_slice(op.data);
        state.log.push(Recorded::Upload {
            buffer: op.buffer,
            offset: op.offset,
            len: op.data.len() as u64,
        });
        Ok(())
    }
}

impl Perform<UploadImage<'_>> for SimGpu {
    fn perform(&mut self, op: UploadImage<'_>) -> Result<(), GpuError> {
        let mut state = self.lock();
        let img =
            validate::image_region(&*state, op.image.raw(), &op.region, op.data.len(), "upload")?;
        let data = state.data(op.image.raw());
        let mut from = 0;
        for (at, len) in region_rows(&img, &op.region) {
            data[at..at + len].copy_from_slice(&op.data[from..from + len]);
            from += len;
        }
        state.log.push(Recorded::UploadImage(op.image));
        Ok(())
    }
}

impl Perform<CopyBuffer> for SimGpu {
    fn perform(&mut self, op: CopyBuffer) -> Result<(), GpuError> {
        let mut state = self.lock();
        validate::copy(&*state, &op)?;
        let (from, len) = (op.src_offset as usize, op.size as usize);
        let bytes = state.data(op.src.raw())[from..from + len].to_vec();
        let to = op.dst_offset as usize;
        state.data(op.dst.raw())[to..to + len].copy_from_slice(&bytes);
        state.log.push(Recorded::Copy(op));
        Ok(())
    }
}

impl Perform<CopyImage> for SimGpu {
    fn perform(&mut self, op: CopyImage) -> Result<(), GpuError> {
        let mut state = self.lock();
        validate::copy_image(&*state, &op)?;
        let (src, dst) = (
            image_of(&state, op.src.raw()),
            image_of(&state, op.dst.raw()),
        );
        let region = |at: &crate::ImageAt| ImageRegion {
            mip: at.mip,
            layer: at.layer,
            layers: op.layers,
            offset: at.offset,
            extent: op.extent,
        };
        let from = region_rows(&src, &region(&op.src_at));
        let to = region_rows(&dst, &region(&op.dst_at));
        let bytes = {
            let data = state.data(op.src.raw());
            from.iter()
                .map(|&(at, len)| data[at..at + len].to_vec())
                .collect::<Vec<_>>()
        };
        let data = state.data(op.dst.raw());
        for ((at, len), row) in to.into_iter().zip(bytes) {
            data[at..at + len].copy_from_slice(&row);
        }
        state.log.push(Recorded::CopyImage(op));
        Ok(())
    }
}

impl Perform<BlitImage> for SimGpu {
    fn perform(&mut self, op: BlitImage) -> Result<(), GpuError> {
        let mut state = self.lock();
        validate::blit(&*state, &op)?;
        let (src, dst) = (
            image_of(&state, op.src.raw()),
            image_of(&state, op.dst.raw()),
        );
        let (pixels, w, h) = decode_slice(&src, state.data(op.src.raw()), op.src_mip, op.src_layer);
        let [sx0, sy0, sx1, sy1] = op.src_rect.map(|v| v as f32);
        let [dx0, dy0, dx1, dy1] = op.dst_rect;
        let (scale_x, scale_y) = (
            (sx1 - sx0) / (dx1 - dx0) as f32,
            (sy1 - sy0) / (dy1 - dy0) as f32,
        );
        let (at, _, dw, _) = area_of(&dst, op.dst_mip, op.dst_layer);
        let n = dst.format.bytes_per_pixel() as usize;
        let data = state.data(op.dst.raw());
        for y in dy0..dy1 {
            for x in dx0..dx1 {
                let u = sx0 + ((x - dx0) as f32 + 0.5) * scale_x;
                let v = sy0 + ((y - dy0) as f32 + 0.5) * scale_y;
                let px = encode_pixel(dst.format, read(&pixels, w, h, u, v, op.filter));
                let i = at + (y * dw + x) as usize * n;
                data[i..i + n].copy_from_slice(&px);
            }
        }
        state.log.push(Recorded::Blit(op));
        Ok(())
    }
}

impl Perform<GenerateMips> for SimGpu {
    fn perform(&mut self, op: GenerateMips) -> Result<(), GpuError> {
        let mut state = self.lock();
        let img = validate::generate_mips(&*state, &op)?;
        let n = img.format.bytes_per_pixel() as usize;
        for level in 1..img.mips {
            let (_, _, sd) = img.extent(level - 1);
            let (dw, dh, dd) = img.extent(level);
            for layer in 0..img.layers {
                let source: Vec<_> = (0..sd)
                    .map(|z| {
                        decode_slice(&img, state.data(op.image.raw()), level - 1, layer * sd + z)
                    })
                    .collect();
                let (sw, sh) = (source[0].1, source[0].2);
                for z in 0..dd {
                    // Halving: each destination slice blends the two (or
                    // one) source slices under it.
                    let zs: Vec<u32> = if sd > dd {
                        std::vec![2 * z, (2 * z + 1).min(sd - 1)]
                    } else {
                        std::vec![z]
                    };
                    let (at, ..) = area_of(&img, level, layer * dd + z);
                    let data = state.data(op.image.raw());
                    for y in 0..dh {
                        for x in 0..dw {
                            let u = (x as f32 + 0.5) * sw as f32 / dw as f32;
                            let v = (y as f32 + 0.5) * sh as f32 / dh as f32;
                            let mut sum = [0.0; 4];
                            for &zi in &zs {
                                let (pixels, w, h) = &source[zi as usize];
                                let px = read(pixels, *w, *h, u, v, Filter::Linear);
                                for k in 0..4 {
                                    sum[k] += px[k] / zs.len() as f32;
                                }
                            }
                            let i = at + (y * dw + x) as usize * n;
                            data[i..i + n].copy_from_slice(&encode_pixel(img.format, sum));
                        }
                    }
                }
            }
        }
        state.log.push(Recorded::GenerateMips(op.image));
        Ok(())
    }
}

impl Perform<FillBuffer> for SimGpu {
    fn perform(&mut self, op: FillBuffer) -> Result<(), GpuError> {
        let mut state = self.lock();
        validate::fill(&*state, &op)?;
        let (at, len) = (op.offset as usize, op.size as usize);
        for chunk in state.data(op.buffer.raw())[at..at + len].chunks_mut(4) {
            chunk.copy_from_slice(&op.value.to_le_bytes());
        }
        state.log.push(Recorded::Fill(op));
        Ok(())
    }
}

impl Perform<Dispatch<'_>> for SimGpu {
    fn perform(&mut self, op: Dispatch<'_>) -> Result<(), GpuError> {
        let mut state = self.lock();
        validate::dispatch(&*state, &op)?;
        state.dispatch(op.program, op.bindings, op.push, op.groups);
        Ok(())
    }
}

impl Perform<DispatchIndirect<'_>> for SimGpu {
    fn perform(&mut self, op: DispatchIndirect<'_>) -> Result<(), GpuError> {
        let mut state = self.lock();
        validate::dispatch_indirect(&*state, &op)?;
        let at = op.offset as usize;
        let groups = words(&state.data(op.buffer.raw())[at..at + 12]);
        state.dispatch(
            op.program,
            op.bindings,
            op.push,
            [groups[0], groups[1], groups[2]],
        );
        Ok(())
    }
}

/// Little-endian `u32`s.
fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect()
}

impl State {
    /// A program's label and constants.
    fn program_of(&self, program: Program) -> (String, Vec<Constant>) {
        match &self.res[&program.raw()].info {
            ResInfo::Program {
                label, constants, ..
            } => (label.clone(), constants.clone()),
            _ => unreachable!("validated as a program"),
        }
    }

    fn dispatch(&mut self, program: Program, bindings: &[Bind], push: &[u8], groups: [u32; 3]) {
        let (label, constants) = self.program_of(program);
        self.log.push(Recorded::Dispatch {
            label: label.clone(),
            groups,
        });
        self.run(
            label,
            RunSetup {
                constants,
                bindings,
                push,
                groups,
                draw: None,
            },
        );
    }
}

/// `rgba`, in [0, 1] and linear, as one pixel of `format`.
pub(crate) fn clear_pixel(format: Format, rgba: [f32; 4]) -> Vec<u8> {
    let unorm = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    let unorm16 = |c: f32| ((c.clamp(0.0, 1.0) * 65535.0).round() as u16).to_le_bytes();
    let snorm = |c: f32| (c.clamp(-1.0, 1.0) * 127.0).round() as i8 as u8;
    let srgb = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        let s = if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        unorm(s)
    };
    let half = |c: f32| f16_bits(c).to_le_bytes();
    let [r, g, b, a] = rgba;
    let bytes = |parts: &[&[u8]]| parts.concat();
    match format {
        Format::R8Unorm => std::vec![unorm(r)],
        Format::Rg8Unorm => std::vec![unorm(r), unorm(g)],
        Format::Rgba8Unorm => std::vec![unorm(r), unorm(g), unorm(b), unorm(a)],
        Format::Rgba8Snorm => std::vec![snorm(r), snorm(g), snorm(b), snorm(a)],
        Format::Bgra8Unorm => std::vec![unorm(b), unorm(g), unorm(r), unorm(a)],
        Format::Rgba8Srgb => std::vec![srgb(r), srgb(g), srgb(b), unorm(a)],
        Format::Bgra8Srgb => std::vec![srgb(b), srgb(g), srgb(r), unorm(a)],
        Format::R8Uint => std::vec![r as u8],
        Format::Rgba8Uint => std::vec![r as u8, g as u8, b as u8, a as u8],
        Format::R16Unorm => unorm16(r).to_vec(),
        Format::Rgba16Unorm => bytes(&[&unorm16(r), &unorm16(g), &unorm16(b), &unorm16(a)]),
        Format::R16Float => half(r).to_vec(),
        Format::Rg16Float => bytes(&[&half(r), &half(g)]),
        Format::Rgba16Float => bytes(&[&half(r), &half(g), &half(b), &half(a)]),
        Format::R16Uint => (r as u16).to_le_bytes().to_vec(),
        Format::R32Float => r.to_le_bytes().to_vec(),
        Format::Rg32Float => bytes(&[&r.to_le_bytes(), &g.to_le_bytes()]),
        Format::Rgba32Float => rgba.iter().flat_map(|c| c.to_le_bytes()).collect(),
        Format::R32Uint => (r as u32).to_le_bytes().to_vec(),
        Format::Rg32Uint => bytes(&[&(r as u32).to_le_bytes(), &(g as u32).to_le_bytes()]),
        Format::Rgba32Uint => rgba
            .iter()
            .flat_map(|c| (*c as u32).to_le_bytes())
            .collect(),
        Format::R32Sint => (r as i32).to_le_bytes().to_vec(),
        Format::Rgb10A2Unorm => {
            let ten = |c: f32| (c.clamp(0.0, 1.0) * 1023.0).round() as u32;
            let two = (a.clamp(0.0, 1.0) * 3.0).round() as u32;
            (ten(r) | ten(g) << 10 | ten(b) << 20 | two << 30)
                .to_le_bytes()
                .to_vec()
        }
        Format::Rg11B10Float => {
            // An unsigned small float is a half's exponent and the top of
            // its mantissa.
            let small = |c: f32, bits: u32| u32::from(f16_bits(c.max(0.0))) >> (15 - bits);
            (small(r, 11) | small(g, 11) << 11 | small(b, 10) << 22)
                .to_le_bytes()
                .to_vec()
        }
        f if f.is_depth() => depth_pixel(f, r, 0),
        // Compressed formats are not drawn into, so never cleared.
        _ => std::vec![0; format.block().2 as usize],
    }
}

/// `depth` and `stencil` as one pixel of the depth format `format`, laid
/// out as [`Run::depth`] says.
pub(crate) fn depth_pixel(format: Format, depth: f32, stencil: u32) -> Vec<u8> {
    let d = depth.clamp(0.0, 1.0);
    match format {
        Format::D16Unorm => ((d * 65535.0).round() as u16).to_le_bytes().to_vec(),
        Format::D32Float => d.to_le_bytes().to_vec(),
        Format::D24UnormS8Uint => (((d * 16_777_215.0).round() as u32) | (stencil & 0xff) << 24)
            .to_le_bytes()
            .to_vec(),
        Format::D32FloatS8Uint => {
            let mut px = d.to_le_bytes().to_vec();
            px.push(stencil as u8);
            px
        }
        _ => unreachable!("a depth format"),
    }
}

/// An `f32` as IEEE half-precision bits, rounding to nearest.
fn f16_bits(x: f32) -> u16 {
    let bits = x.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mant = bits & 0x7f_ffff;
    if exp == 0xff {
        // Infinity or NaN.
        return sign | 0x7c00 | if mant != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (mant | 0x80_0000) >> (1 - e);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    sign | ((e as u16) << 10) | ((mant + 0x1000) >> 13) as u16
}

impl Perform<Draw<'_>> for SimGpu {
    fn perform(&mut self, op: Draw<'_>) -> Result<(), GpuError> {
        let mut state = self.lock();
        validate::draw(&*state, &op)?;
        let (label, constants) = state.program_of(op.program);
        for t in op.targets.iter().flatten() {
            if let Load::Clear(rgba) = t.load {
                let img = image_of(&state, t.image.raw());
                let (at, len, _, _) = area_of(&img, t.mip, t.layer);
                let pixel = clear_pixel(img.format, rgba);
                for chunk in state.data(t.image.raw())[at..at + len].chunks_mut(pixel.len()) {
                    chunk.copy_from_slice(&pixel);
                }
            }
        }
        if let Some(d) = op.depth
            && let DepthLoad::Clear { depth, stencil } = d.load
        {
            let img = image_of(&state, d.image.raw());
            let (at, len, _, _) = area_of(&img, d.mip, d.layer);
            let pixel = depth_pixel(img.format, depth, stencil);
            for chunk in state.data(d.image.raw())[at..at + len].chunks_mut(pixel.len()) {
                chunk.copy_from_slice(&pixel);
            }
        }
        let first = op
            .targets
            .iter()
            .flatten()
            .map(|t| t.image)
            .chain(op.depth.map(|d| d.image))
            .next()
            .expect("validated: a draw writes something");
        // An indirect draw is each of its commands in turn.
        let draws: Vec<Draw<'_>> = match op.indirect {
            None => std::vec![op],
            Some(ind) => {
                let indexed = op.index.is_some();
                let size = Indirect::command_size(indexed) as usize;
                let step = ind.step(indexed) as usize;
                let data = state.data(ind.buffer.raw());
                (0..ind.count as usize)
                    .map(|i| {
                        let at = ind.offset as usize + i * step;
                        let w = words(&data[at..at + size]);
                        let mut d = op;
                        d.indirect = None;
                        (d.vertex_count, d.instance_count, d.first) = (w[0], w[1], w[2]);
                        if indexed {
                            (d.base_vertex, d.first_instance) = (w[3] as i32, w[4]);
                        } else {
                            d.first_instance = w[3];
                        }
                        d
                    })
                    .collect()
            }
        };
        let mut passed = 0;
        for d in &draws {
            state.log.push(Recorded::Draw {
                label: label.clone(),
                target: first,
                vertex_count: d.vertex_count,
                instance_count: d.instance_count,
            });
            passed += state.run(
                label.clone(),
                RunSetup {
                    constants: constants.clone(),
                    bindings: op.bindings,
                    push: op.push,
                    groups: [0; 3],
                    draw: Some(d),
                },
            );
        }
        if let Some((q, index)) = op.occlusion {
            state.record_query(q.raw(), index, passed);
        }
        // Multisampled targets are kept one sample per pixel: resolving
        // copies them.
        for t in op.targets.iter().flatten() {
            if let Some(r) = t.resolve {
                let img = image_of(&state, t.image.raw());
                let (at, len, _, _) = area_of(&img, t.mip, t.layer);
                let pixels = state.data(t.image.raw())[at..at + len].to_vec();
                state.data(r.raw())[..len].copy_from_slice(&pixels);
            }
        }
        Ok(())
    }
}

impl Perform<Download<'_>> for SimGpu {
    fn perform(&mut self, op: Download<'_>) -> Result<(), GpuError> {
        let mut state = self.lock();
        let len = op.out.len() as u64;
        validate::download(&*state, op.buffer.raw(), op.offset, len)?;
        let start = op.offset as usize;
        op.out
            .copy_from_slice(&state.data(op.buffer.raw())[start..start + op.out.len()]);
        state.log.push(Recorded::Download {
            buffer: op.buffer,
            offset: op.offset,
            len,
        });
        Ok(())
    }
}

impl Perform<DownloadImage<'_>> for SimGpu {
    fn perform(&mut self, op: DownloadImage<'_>) -> Result<(), GpuError> {
        let mut state = self.lock();
        let img = validate::image_region(
            &*state,
            op.image.raw(),
            &op.region,
            op.out.len(),
            "download",
        )?;
        let data = state.data(op.image.raw());
        let mut to = 0;
        for (at, len) in region_rows(&img, &op.region) {
            op.out[to..to + len].copy_from_slice(&data[at..at + len]);
            to += len;
        }
        state.log.push(Recorded::DownloadImage(op.image));
        Ok(())
    }
}

/// A submission on a [`SimGpu`]: nothing in it waits, so it is always
/// done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SimSubmission;

impl Perform<Opening<Submit<SimSubmission>>> for SimGpu {
    fn perform(&mut self, _: Opening<Submit<SimSubmission>>) -> Result<SimSubmission, GpuError> {
        self.lock().log.push(Recorded::Submit);
        Ok(SimSubmission)
    }
}

impl Perform<Closing<Submit<SimSubmission>>> for SimGpu {
    fn perform(&mut self, _: Closing<Submit<SimSubmission>>) -> Result<(), GpuError> {
        Ok(())
    }
}

impl Inspect<IsDone> for SimSubmission {
    fn inspect(&self, _: IsDone) -> Result<bool, GpuError> {
        Ok(true)
    }
}

impl seam_core::Capability for SimSubmission {}

impl Perform<Finish> for SimGpu {
    fn perform(&mut self, _: Finish) -> Result<(), GpuError> {
        self.lock().log.push(Recorded::Finish);
        Ok(())
    }
}

impl Inspect<DeviceInfo> for SimGpu {
    fn inspect(&self, _: DeviceInfo) -> GpuInfo {
        GpuInfo {
            name: "seam simulated GPU".into(),
            kind: GpuKind::Other,
            api_version: (1, 3, 0),
            features: GpuFeatures::ALL,
            limits: Limits::MINIMUM,
        }
    }
}

// The lifts give it the async forms.
impl seam_core::Capability for SimGpu {}
