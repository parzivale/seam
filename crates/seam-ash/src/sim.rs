//! [`SimGpu`]: a GPU in memory, for tests.

use std::boxed::Box;
use std::collections::{BTreeMap, HashMap};
use std::string::String;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

use seam_core::{Closing, Inspect, Opening, Perform};

use crate::validate::{self, Lookup};
use crate::{
    AddressMode, Bind, Buffer, CopyBuffer, CreateBuffer, CreateImage, CreateProgram, CreateSampler,
    DeviceInfo, Dispatch, Download, DownloadImage, Draw, Filter, Finish, Format, GpuError, GpuInfo,
    GpuKind, Image, Import, IsDone, Program, ProgramKind, ResInfo, Sampler, Submit, Upload,
    UploadImage,
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
        /// Into which image.
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
    groups: [u32; 3],
    push: &'a [u8],
    /// For each binding, the slot holding its resource's bytes (`None`
    /// for a sampler, which has none).
    bound: Vec<Option<usize>>,
    /// For each binding, its sampler, if it is a sampled image or one.
    samplers: Vec<Option<CreateSampler>>,
    slots: Vec<Slot>,
    target: Option<usize>,
    vertices: Option<usize>,
    vertex_count: u32,
    instance_count: u32,
}

struct Slot {
    id: u64,
    data: Vec<u8>,
    size: Option<(u32, u32)>,
    format: Option<Format>,
}

impl Run<'_> {
    /// The program's label.
    pub fn label(&self) -> &str {
        self.label
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
        self.bound[i].and_then(|s| self.slots[s].size)
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
        let slot = &self.slots[self.slot(i)];
        let (w, h) = slot.size.expect("seam-ash: the binding is not an image");
        assert!(
            x < w && y < h,
            "seam-ash: texel {x}, {y} is outside a {w}x{h} image"
        );
        let format = slot.format.expect("an image has a format");
        let n = format.bytes_per_pixel() as usize;
        let at = (y as usize * w as usize + x as usize) * n;
        decode_pixel(format, &slot.data[at..at + n])
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

    /// For a draw: the target image's pixels (already cleared, if the draw
    /// clears), and its width and height.
    ///
    /// # Panics
    ///
    /// If this is a dispatch.
    pub fn target(&mut self) -> (&mut [u8], u32, u32) {
        let slot = self.target.expect("seam-ash: a dispatch has no target");
        let (w, h) = self.slots[slot].size.expect("a target is an image");
        (&mut self.slots[slot].data, w, h)
    }

    /// For a draw: the vertex buffer's bytes, if any.
    pub fn vertices(&self) -> Option<&[u8]> {
        self.vertices.map(|s| self.slots[s].data.as_slice())
    }

    /// For a draw: how many vertices and instances.
    pub fn counts(&self) -> (u32, u32) {
        (self.vertex_count, self.instance_count)
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
    let srgb = |b: u8| {
        let c = unorm(b);
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let half = |i: usize| f16_value(u16::from_le_bytes([px[i], px[i + 1]]));
    let float = |i: usize| f32::from_le_bytes([px[i], px[i + 1], px[i + 2], px[i + 3]]);
    match format {
        Format::Rgba8Unorm => [unorm(px[0]), unorm(px[1]), unorm(px[2]), unorm(px[3])],
        Format::Bgra8Unorm => [unorm(px[2]), unorm(px[1]), unorm(px[0]), unorm(px[3])],
        Format::Rgba8Srgb => [srgb(px[0]), srgb(px[1]), srgb(px[2]), unorm(px[3])],
        Format::Bgra8Srgb => [srgb(px[2]), srgb(px[1]), srgb(px[0]), unorm(px[3])],
        Format::R8Unorm => [unorm(px[0]), 0.0, 0.0, 1.0],
        Format::Rgba16Float => [half(0), half(2), half(4), half(6)],
        Format::Rgba32Float => [float(0), float(4), float(8), float(12)],
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
    fn run(&mut self, label: String, run_setup: RunSetup<'_>) {
        let Some(mut emulator) = self.emulators.remove(&label) else {
            return;
        };
        let mut slots: Vec<Slot> = Vec::new();
        let mut slot_of = |state: &mut State, id: u64| -> usize {
            if let Some(i) = slots.iter().position(|s| s.id == id) {
                return i;
            }
            let entry = state.res.get_mut(&id).expect("checked to exist");
            let (size, format) = match entry.info {
                ResInfo::Image {
                    width,
                    height,
                    format,
                    ..
                } => (Some((width, height)), Some(format)),
                _ => (None, None),
            };
            slots.push(Slot {
                id,
                data: core::mem::take(&mut entry.data),
                size,
                format,
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
        let target = run_setup.target.map(|t| slot_of(self, t.raw()));
        let vertices = run_setup.vertices.map(|v| slot_of(self, v.raw()));
        let mut run = Run {
            label: &label,
            groups: run_setup.groups,
            push: run_setup.push,
            bound,
            samplers,
            slots,
            target,
            vertices,
            vertex_count: run_setup.vertex_count,
            instance_count: run_setup.instance_count,
        };
        emulator(&mut run);
        for slot in run.slots {
            *self.data(slot.id) = slot.data;
        }
        self.emulators.insert(label, emulator);
    }
}

struct RunSetup<'a> {
    bindings: &'a [Bind],
    push: &'a [u8],
    groups: [u32; 3],
    target: Option<Image>,
    vertices: Option<Buffer>,
    vertex_count: u32,
    instance_count: u32,
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
        let len = op.width as usize * op.height as usize * op.format.bytes_per_pixel() as usize;
        let mut state = self.lock();
        let info = ResInfo::Image {
            width: op.width,
            height: op.height,
            format: op.format,
            usage: op.usage,
        };
        let id = state.add(info, len);
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

impl Perform<Opening<CreateProgram<'_>>> for SimGpu {
    fn perform(&mut self, Opening(op, _): Opening<CreateProgram<'_>>) -> Result<Program, GpuError> {
        validate::create_program(&op)?;
        let (compute, target, stride) = match op.kind {
            ProgramKind::Compute { .. } => (true, None, 0),
            ProgramKind::Draw { target, stride, .. } => (false, Some(target), stride),
        };
        let mut state = self.lock();
        let info = ResInfo::Program {
            label: op.label.into(),
            compute,
            bindings: op.bindings.to_vec(),
            push_constants: op.push_constants,
            target,
            stride,
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
            let info = ResInfo::Image {
                width: buf.width,
                height: buf.height,
                format,
                usage: op.usage,
            };
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
        validate::whole_image(&*state, op.image.raw(), op.data.len(), "upload")?;
        state.data(op.image.raw()).copy_from_slice(op.data);
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

impl Perform<Dispatch<'_>> for SimGpu {
    fn perform(&mut self, op: Dispatch<'_>) -> Result<(), GpuError> {
        let mut state = self.lock();
        validate::dispatch(&*state, &op)?;
        let label = validate::program(&*state, op.program.raw())?
            .label
            .to_owned();
        state.log.push(Recorded::Dispatch {
            label: label.clone(),
            groups: op.groups,
        });
        state.run(
            label,
            RunSetup {
                bindings: op.bindings,
                push: op.push,
                groups: op.groups,
                target: None,
                vertices: None,
                vertex_count: 0,
                instance_count: 0,
            },
        );
        Ok(())
    }
}

/// `rgba`, in [0, 1] and linear, as one pixel of `format`.
pub(crate) fn clear_pixel(format: Format, rgba: [f32; 4]) -> Vec<u8> {
    let unorm = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    let srgb = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        let s = if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        unorm(s)
    };
    let [r, g, b, a] = rgba;
    match format {
        Format::Rgba8Unorm => std::vec![unorm(r), unorm(g), unorm(b), unorm(a)],
        Format::Bgra8Unorm => std::vec![unorm(b), unorm(g), unorm(r), unorm(a)],
        Format::Rgba8Srgb => std::vec![srgb(r), srgb(g), srgb(b), unorm(a)],
        Format::Bgra8Srgb => std::vec![srgb(b), srgb(g), srgb(r), unorm(a)],
        Format::R8Unorm => std::vec![unorm(r)],
        Format::Rgba16Float => rgba
            .iter()
            .flat_map(|c| f16_bits(*c).to_le_bytes())
            .collect(),
        Format::Rgba32Float => rgba.iter().flat_map(|c| c.to_le_bytes()).collect(),
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
        let label = validate::program(&*state, op.program.raw())?
            .label
            .to_owned();
        if let Some(rgba) = op.clear {
            let (_, _, format, _) = validate::image(&*state, op.target.raw())?;
            let pixel = clear_pixel(format, rgba);
            for chunk in state.data(op.target.raw()).chunks_mut(pixel.len()) {
                chunk.copy_from_slice(&pixel);
            }
        }
        state.log.push(Recorded::Draw {
            label: label.clone(),
            target: op.target,
            vertex_count: op.vertex_count,
            instance_count: op.instance_count,
        });
        state.run(
            label,
            RunSetup {
                bindings: op.bindings,
                push: op.push,
                groups: [0; 3],
                target: Some(op.target),
                vertices: op.vertices,
                vertex_count: op.vertex_count,
                instance_count: op.instance_count,
            },
        );
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
        validate::whole_image(&*state, op.image.raw(), op.out.len(), "download")?;
        op.out.copy_from_slice(state.data(op.image.raw()));
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
        }
    }
}

// The lifts give it the async forms.
impl seam_core::Capability for SimGpu {}
