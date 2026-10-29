//! [`SimGpu`]: a GPU in memory, for tests.

use std::boxed::Box;
use std::collections::{BTreeMap, HashMap};
use std::string::String;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

use seam_core::{Closing, Inspect, Opening, Perform};

use crate::validate::{self, Lookup};
use crate::{
    Bind, Buffer, CopyBuffer, CreateBuffer, CreateImage, CreateProgram, DeviceInfo, Dispatch,
    Download, DownloadImage, Draw, Finish, Format, GpuError, GpuInfo, GpuKind, Image, IsDone,
    Program, ProgramKind, ResInfo, Submit, Upload, UploadImage, Wait,
};

/// What [`SimGpu`] was asked to do, in order: for tests to assert on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// A buffer was created.
    CreateBuffer(Buffer),
    /// An image was created.
    CreateImage(Image),
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
    /// For each binding, the slot holding its resource's bytes.
    bound: Vec<usize>,
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
        let slot = self.bound[i];
        &mut self.slots[slot].data
    }

    /// The width and height of the image at binding `i`, or `None` for a
    /// buffer.
    pub fn image_size(&self, i: usize) -> Option<(u32, u32)> {
        self.slots[self.bound[i]].size
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
            let size = match entry.info {
                ResInfo::Image { width, height, .. } => Some((width, height)),
                _ => None,
            };
            slots.push(Slot {
                id,
                data: core::mem::take(&mut entry.data),
                size,
            });
            slots.len() - 1
        };
        let bound: Vec<usize> = run_setup
            .bindings
            .iter()
            .map(|b| {
                let id = match *b {
                    Bind::Uniform(x) | Bind::Storage(x) => x.raw(),
                    Bind::Sampled(x) | Bind::StorageImage(x) => x.raw(),
                };
                slot_of(self, id)
            })
            .collect();
        let target = run_setup.target.map(|t| slot_of(self, t.raw()));
        let vertices = run_setup.vertices.map(|v| slot_of(self, v.raw()));
        let mut run = Run {
            label: &label,
            groups: run_setup.groups,
            push: run_setup.push,
            bound,
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
    CreateProgram<'_> => Program;
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

impl Perform<Wait> for SimSubmission {
    fn perform(&mut self, _: Wait) -> Result<(), GpuError> {
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
