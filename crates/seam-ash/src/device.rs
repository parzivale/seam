//! [`Ash`]: the operations on real Vulkan, through ash.
//!
//! Every Vulkan call is `unsafe`: its rules are the caller's to keep. This
//! module keeps them by construction:
//!
//! - Every operation is checked first (`validate`), so no call gets a
//!   destroyed handle, an out-of-range region, or a binding its layout
//!   does not declare.
//! - All state is behind one mutex, so Vulkan's externally synchronized
//!   objects (the queue, the command pool, descriptor pools) are only ever
//!   used by one thread at a time. The waits happen outside the lock.
//! - A resource is destroyed only once no submitted work can still use it:
//!   closing its scope defers the destruction until the last batch that
//!   used it has finished.
//! - A fence is destroyed only once it has signalled and nobody (a
//!   blocking caller, or the async waiter thread) still waits on it.
//!
//! Work is recorded into one command buffer and submitted when something
//! waits. Each operation declares what it reads and writes, and
//! `Dev::uses` inserts the barriers and image layout transitions between
//! them, tracking each resource's last write, its reads since, and which
//! stages have seen the write.

use core::ffi::c_void;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use core::time::Duration;
use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::format;
use std::string::{String, ToString};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};
use std::vec::Vec;

use ash::vk;
use seam_core::{Closing, Inspect, Opening, Perform};

use crate::validate::{self, Lookup};
use crate::{
    AttrFormat, Bind, BindingKind, Blend, Buffer, BufferUsage, CopyBuffer, CreateBuffer,
    CreateImage, CreateProgram, DeviceInfo, Dispatch, Download, DownloadImage, Draw, Finish,
    Format, GpuError, GpuInfo, GpuKind, Image, ImageUsage, Import, IsDone, Program, ProgramKind,
    ResInfo, Submit, Upload, UploadImage,
};

/// How [`Ash::with`] sets Vulkan up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AshOptions {
    /// Turn on the Khronos validation layer, with synchronization
    /// validation, and keep what it reports for
    /// [`Ash::validation_messages`]. Fails with `Unsupported` if the layer
    /// is not installed.
    pub validation: bool,
    /// Prefer this kind of GPU, when there is a choice. Without it, a
    /// discrete GPU is preferred, then integrated, virtual and software.
    pub prefer: Option<GpuKind>,
    /// How long a wait keeps checking whether the GPU has finished before
    /// it goes to sleep (blocking) or hands the wait to the waiter thread
    /// (async). Checking notices completion at once, where a sleeping
    /// thread must be woken; it costs the CPU it spins for. Set it near
    /// the time of the GPU work you wait on to cut waiting latency, or to
    /// zero to never spin. 20 µs by default.
    pub spin: Duration,
    /// The stack size of the thread that waits for async operations: it
    /// only waits on fences, so it needs little. 64 KiB by default.
    pub waiter_stack: usize,
}

impl Default for AshOptions {
    fn default() -> Self {
        Self {
            validation: false,
            prefer: None,
            spin: Duration::from_micros(20),
            waiter_stack: 64 * 1024,
        }
    }
}

/// Real Vulkan 1.3, through ash.
///
/// It performs every operation in this crate, in two forms from one
/// implementation: `Perform` (and `Inspect`) block the calling thread when
/// an operation waits for the GPU; `AsyncPerform` and `SendPerform` await
/// instead, through a waiter thread started the first time an async
/// operation waits, under any executor. Everything but the waiting is the
/// same sync code. It needs a device with Vulkan 1.3's dynamic rendering
/// and synchronization2, and a queue that does both graphics and compute.
///
/// Every clone is a handle to the same device; it is destroyed with the
/// last.
#[derive(Clone)]
pub struct Ash {
    shared: Arc<Shared>,
}

impl core::fmt::Debug for Ash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ash")
            .field("device", &self.shared.info.name)
            .finish_non_exhaustive()
    }
}

struct Shared {
    dev: Mutex<Dev>,
    /// For waiting outside the lock.
    device: ash::Device,
    info: GpuInfo,
    messages: Arc<Mutex<Vec<String>>>,
    waiter: OnceLock<Waiter>,
    spin: Duration,
    waiter_stack: usize,
}

/// The thread that waits on fences for async operations.
struct Waiter {
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

struct Job {
    fence: vk::Fence,
    signal: Arc<Signal>,
}

/// The async waits on one batch: whether it is done, and the wakers to
/// call when it is. One per batch, made by its first async wait.
#[derive(Default)]
struct Signal {
    state: Mutex<SignalState>,
}

#[derive(Default)]
struct SignalState {
    done: Option<Result<(), GpuError>>,
    wakers: Vec<Waker>,
}

impl Signal {
    fn lock(&self) -> MutexGuard<'_, SignalState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_done(&self) -> bool {
        self.lock().done.is_some()
    }

    fn complete(&self, result: Result<(), GpuError>) {
        let wakers = {
            let mut state = self.lock();
            state.done = Some(result);
            core::mem::take(&mut state.wakers)
        };
        for waker in wakers {
            waker.wake();
        }
    }
}

/// Waits for a [`Signal`].
struct FenceWait(Arc<Signal>);

impl Future for FenceWait {
    type Output = Result<(), GpuError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.0.lock();
        match &state.done {
            Some(result) => Poll::Ready(result.clone()),
            None => {
                let waker = cx.waker();
                if !state.wakers.iter().any(|w| w.will_wake(waker)) {
                    state.wakers.push(waker.clone());
                }
                Poll::Pending
            }
        }
    }
}

fn vk_error(r: vk::Result) -> GpuError {
    match r {
        vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => {
            GpuError::OutOfMemory
        }
        vk::Result::ERROR_DEVICE_LOST => GpuError::DeviceLost,
        vk::Result::ERROR_FORMAT_NOT_SUPPORTED
        | vk::Result::ERROR_FEATURE_NOT_PRESENT
        | vk::Result::ERROR_EXTENSION_NOT_PRESENT
        | vk::Result::ERROR_LAYER_NOT_PRESENT
        | vk::Result::ERROR_INCOMPATIBLE_DRIVER => GpuError::Unsupported(format!("{r}")),
        other => GpuError::Vulkan(other.as_raw()),
    }
}

/// A host-visible buffer for moving data in or out.
struct Staging {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
}

enum Kind {
    Buffer {
        buffer: vk::Buffer,
        memory: vk::DeviceMemory,
    },
    Image {
        image: vk::Image,
        memory: vk::DeviceMemory,
        view: vk::ImageView,
        layout: vk::ImageLayout,
        foreign: Foreign,
    },
    Program {
        pipeline: vk::Pipeline,
        layout: vk::PipelineLayout,
        set_layout: vk::DescriptorSetLayout,
    },
}

/// Whether an image's memory belongs to something outside this device (an
/// import), which Vulkan has hand over by a queue family ownership
/// transfer: acquired before the first use, released before the owner gets
/// it back.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(any(drm, windows)), allow(dead_code))]
enum Foreign {
    /// Ours alone.
    No,
    /// Imported from this queue family (`QUEUE_FAMILY_FOREIGN_EXT` or
    /// `QUEUE_FAMILY_EXTERNAL`), not yet acquired.
    Acquire(u32),
    /// Acquired from this family; released back to it when the scope ends.
    Held(u32),
}

/// What has happened to a resource since its last write, for barriers.
#[derive(Default)]
struct Track {
    /// The last write's stages and access.
    write: Option<(vk::PipelineStageFlags2, vk::AccessFlags2)>,
    /// The stages that have read it since.
    reads: vk::PipelineStageFlags2,
    /// The stages the last write has been made visible to.
    visible: vk::PipelineStageFlags2,
}

struct Res {
    info: ResInfo,
    kind: Kind,
    track: Track,
    /// The last batch that used it (0: none).
    last_use: u64,
}

/// One use of a resource by the operation being recorded.
#[derive(Clone, Copy)]
struct Use {
    id: u64,
    stage: vk::PipelineStageFlags2,
    access: vk::AccessFlags2,
    write: bool,
    layout: Option<vk::ImageLayout>,
}

/// Work being recorded.
struct Batch {
    seq: u64,
    cmd: vk::CommandBuffer,
    staging: Vec<Staging>,
    pools: Vec<vk::DescriptorPool>,
}

/// Work submitted and not yet known to be finished.
struct InFlight {
    seq: u64,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    staging: Vec<Staging>,
    pools: Vec<vk::DescriptorPool>,
    /// Blocking callers and submissions waiting on the fence, outside the
    /// lock.
    holds: usize,
    /// The async waits on the fence, through the waiter thread.
    signal: Option<Arc<Signal>>,
}

/// A download submitted: its fence to wait on, then its staging to read.
/// Its batch is held from the moment it is made, under the same lock, so
/// the fence outlives the wait; [`Dev::end_wait`] (or dropping the
/// [`InProgress`] around it) lets go.
struct Pending {
    seq: u64,
    fence: vk::Fence,
    staging: Option<Staging>,
}

/// Optional device extensions, on where the device has them.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools)]
struct Extensions {
    /// `VK_KHR_external_memory_fd` and `VK_EXT_external_memory_dma_buf`.
    #[cfg(drm)]
    dmabuf: Option<ash::khr::external_memory_fd::Device>,
    /// `VK_EXT_image_drm_format_modifier`: dmabufs imported as images,
    /// without a copy.
    #[cfg(drm)]
    modifiers: bool,
    /// `VK_EXT_queue_family_foreign`.
    #[cfg(drm)]
    foreign: bool,
    /// `VK_KHR_external_memory_win32`.
    #[cfg(windows)]
    win32: Option<ash::khr::external_memory_win32::Device>,
    /// `VK_EXT_metal_objects`.
    #[cfg(target_vendor = "apple")]
    metal: bool,
}

/// The optional device extensions wanted, by name, for this platform.
fn wanted_extensions() -> Vec<&'static core::ffi::CStr> {
    // A portability implementation (MoltenVK) must have this turned on.
    #[allow(unused_mut)]
    let mut names = std::vec![vk::KHR_PORTABILITY_SUBSET_NAME];
    #[cfg(drm)]
    names.extend([
        vk::KHR_EXTERNAL_MEMORY_FD_NAME,
        vk::EXT_EXTERNAL_MEMORY_DMA_BUF_NAME,
        vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME,
        vk::EXT_QUEUE_FAMILY_FOREIGN_NAME,
    ]);
    #[cfg(windows)]
    names.push(vk::KHR_EXTERNAL_MEMORY_WIN32_NAME);
    #[cfg(target_vendor = "apple")]
    names.push(vk::EXT_METAL_OBJECTS_NAME);
    names
}

/// How many descriptor sets one pool holds.
const SETS_PER_POOL: u32 = 64;

struct Dev {
    _entry: ash::Entry,
    instance: ash::Instance,
    debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    /// What the debug callback writes into, kept alive until after the
    /// messenger is destroyed.
    _messages: Option<Arc<Mutex<Vec<String>>>>,
    physical: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    /// The queue's family, for ownership transfers.
    family: u32,
    /// The optional extensions turned on, for imports.
    ext: Extensions,
    mem: vk::PhysicalDeviceMemoryProperties,
    pool: vk::CommandPool,
    sampler: vk::Sampler,
    next_id: u64,
    res: HashMap<u64, Res>,
    batch: Option<Batch>,
    in_flight: VecDeque<InFlight>,
    /// Resources whose scopes closed, destroyed once their last batch is.
    dead: Vec<(u64, Kind)>,
    /// Staging of downloads whose futures were dropped mid-wait, freed once
    /// their batch is done.
    orphans: Vec<(u64, Staging)>,
    next_seq: u64,
    completed: u64,
}

impl Lookup for Dev {
    fn info(&self, id: u64) -> Option<&ResInfo> {
        self.res.get(&id).map(|r| &r.info)
    }
}

unsafe extern "system" fn on_message(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    user: *mut c_void,
) -> vk::Bool32 {
    // SAFETY: Vulkan passes the callback data it was given, valid for the
    // call, and `user` points into the `Arc` the messenger was created
    // with, which `Dev` keeps alive until after destroying the messenger.
    unsafe {
        if severity.intersects(
            vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
        ) && !data.is_null()
        {
            let text = (*data)
                .message_as_c_str()
                .map(|m| m.to_string_lossy().into_owned())
                .unwrap_or_default();
            let messages = &*(user as *const Mutex<Vec<String>>);
            messages
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(text);
        }
    }
    vk::FALSE
}

fn kind_of(t: vk::PhysicalDeviceType) -> GpuKind {
    match t {
        vk::PhysicalDeviceType::DISCRETE_GPU => GpuKind::Discrete,
        vk::PhysicalDeviceType::INTEGRATED_GPU => GpuKind::Integrated,
        vk::PhysicalDeviceType::VIRTUAL_GPU => GpuKind::Virtual,
        vk::PhysicalDeviceType::CPU => GpuKind::Cpu,
        _ => GpuKind::Other,
    }
}

fn rank(kind: GpuKind, prefer: Option<GpuKind>) -> u32 {
    if Some(kind) == prefer {
        return 0;
    }
    match kind {
        GpuKind::Discrete => 1,
        GpuKind::Integrated => 2,
        GpuKind::Virtual => 3,
        GpuKind::Cpu => 4,
        GpuKind::Other => 5,
    }
}

fn vk_format(f: Format) -> vk::Format {
    match f {
        Format::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        Format::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        Format::Rgba8Srgb => vk::Format::R8G8B8A8_SRGB,
        Format::Bgra8Srgb => vk::Format::B8G8R8A8_SRGB,
        Format::R8Unorm => vk::Format::R8_UNORM,
        Format::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
        Format::Rgba32Float => vk::Format::R32G32B32A32_SFLOAT,
    }
}

fn attr_format(f: AttrFormat) -> vk::Format {
    match f {
        AttrFormat::F32 => vk::Format::R32_SFLOAT,
        AttrFormat::F32x2 => vk::Format::R32G32_SFLOAT,
        AttrFormat::F32x3 => vk::Format::R32G32B32_SFLOAT,
        AttrFormat::F32x4 => vk::Format::R32G32B32A32_SFLOAT,
        AttrFormat::U32 => vk::Format::R32_UINT,
    }
}

fn descriptor_type(k: BindingKind) -> vk::DescriptorType {
    match k {
        BindingKind::UniformBuffer => vk::DescriptorType::UNIFORM_BUFFER,
        BindingKind::StorageBuffer => vk::DescriptorType::STORAGE_BUFFER,
        BindingKind::SampledImage => vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
        BindingKind::StorageImage => vk::DescriptorType::STORAGE_IMAGE,
    }
}

const COLOR: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

const COLOR_LAYER: vk::ImageSubresourceLayers = vk::ImageSubresourceLayers {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    mip_level: 0,
    base_array_layer: 0,
    layer_count: 1,
};

impl Dev {
    fn open(
        options: &AshOptions,
        messages: &Arc<Mutex<Vec<String>>>,
    ) -> Result<(Self, GpuInfo), GpuError> {
        // SAFETY: loading the Vulkan library runs its initialisation; the
        // system's loader is trusted, as any Vulkan program trusts it.
        let entry = unsafe { ash::Entry::load() }
            .map_err(|e| GpuError::Unsupported(format!("no Vulkan loader: {e}")))?;
        let validation_layer = c"VK_LAYER_KHRONOS_validation";
        let mut layers: Vec<*const core::ffi::c_char> = Vec::new();
        let mut extensions: Vec<*const core::ffi::c_char> = Vec::new();
        if options.validation {
            // SAFETY: a plain query.
            let available =
                unsafe { entry.enumerate_instance_layer_properties() }.map_err(vk_error)?;
            let found = available
                .iter()
                .any(|l| l.layer_name_as_c_str().is_ok_and(|n| n == validation_layer));
            if !found {
                return Err(GpuError::Unsupported(
                    "the Khronos validation layer is not installed".into(),
                ));
            }
            layers.push(validation_layer.as_ptr());
            extensions.push(vk::EXT_DEBUG_UTILS_NAME.as_ptr());
            extensions.push(vk::EXT_VALIDATION_FEATURES_NAME.as_ptr());
        }
        // SAFETY: a plain query.
        let instance_extensions =
            unsafe { entry.enumerate_instance_extension_properties(None) }.map_err(vk_error)?;
        // Lists portability implementations (MoltenVK) too, where the
        // loader has them.
        let portability = instance_extensions.iter().any(|e| {
            e.extension_name_as_c_str()
                .is_ok_and(|n| n == vk::KHR_PORTABILITY_ENUMERATION_NAME)
        });
        if portability {
            extensions.push(vk::KHR_PORTABILITY_ENUMERATION_NAME.as_ptr());
        }
        let app = vk::ApplicationInfo::default()
            .application_name(c"seam")
            .api_version(vk::API_VERSION_1_3);
        let enables = [vk::ValidationFeatureEnableEXT::SYNCHRONIZATION_VALIDATION];
        let mut features =
            vk::ValidationFeaturesEXT::default().enabled_validation_features(&enables);
        let mut create = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_layer_names(&layers)
            .enabled_extension_names(&extensions);
        if portability {
            create = create.flags(vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR);
        }
        if options.validation {
            create = create.push_next(&mut features);
        }
        // SAFETY: the create info and everything it points to live until
        // the call returns.
        let instance = unsafe { entry.create_instance(&create, None) }.map_err(vk_error)?;

        let (debug, kept_messages) = if options.validation {
            let utils = ash::ext::debug_utils::Instance::new(&entry, &instance);
            let kept = Arc::clone(messages);
            let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                        | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION,
                )
                .pfn_user_callback(Some(on_message))
                .user_data(Arc::as_ptr(&kept) as *mut c_void);
            // SAFETY: the user data points into `kept`, held by `Dev`
            // until after the messenger is destroyed.
            let messenger = unsafe { utils.create_debug_utils_messenger(&info, None) };
            match messenger {
                Ok(m) => (Some((utils, m)), Some(kept)),
                Err(e) => {
                    // SAFETY: nothing was created from the instance yet.
                    unsafe { instance.destroy_instance(None) };
                    return Err(vk_error(e));
                }
            }
        } else {
            (None, None)
        };

        let destroy_instance =
            |debug: &Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>| {
                // SAFETY: called only on failure, before any device exists.
                unsafe {
                    if let Some((utils, m)) = debug {
                        utils.destroy_debug_utils_messenger(*m, None);
                    }
                    instance.destroy_instance(None);
                }
            };

        let picked = Self::pick(&instance, options.prefer);
        let Some((physical, family, props)) = picked else {
            destroy_instance(&debug);
            return Err(GpuError::Unsupported(
                "no Vulkan 1.3 device with dynamic rendering, synchronization2 and a graphics and compute queue".into(),
            ));
        };

        let priorities = [1.0f32];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&priorities)];
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default()
            .dynamic_rendering(true)
            .synchronization2(true);
        // SAFETY: a plain query.
        let available =
            unsafe { instance.enumerate_device_extension_properties(physical) }.unwrap_or_default();
        let has = |name: &core::ffi::CStr| {
            available
                .iter()
                .any(|e| e.extension_name_as_c_str().is_ok_and(|n| n == name))
        };
        let enabled: Vec<&core::ffi::CStr> =
            wanted_extensions().into_iter().filter(|n| has(n)).collect();
        let enabled_ptrs: Vec<*const core::ffi::c_char> =
            enabled.iter().map(|n| n.as_ptr()).collect();
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(&enabled_ptrs)
            .push_next(&mut f13);
        // SAFETY: `physical` came from this instance, and supports the
        // features requested (checked in `pick`).
        let device = match unsafe { instance.create_device(physical, &device_info, None) } {
            Ok(d) => d,
            Err(e) => {
                destroy_instance(&debug);
                return Err(vk_error(e));
            }
        };
        // SAFETY: queue 0 of the family was requested above.
        let queue = unsafe { device.get_device_queue(family, 0) };
        let on = |name: &core::ffi::CStr| enabled.contains(&name);
        #[allow(unused_mut)]
        let mut ext = Extensions::default();
        #[cfg(drm)]
        {
            if on(vk::KHR_EXTERNAL_MEMORY_FD_NAME) && on(vk::EXT_EXTERNAL_MEMORY_DMA_BUF_NAME) {
                ext.dmabuf = Some(ash::khr::external_memory_fd::Device::new(
                    &instance, &device,
                ));
            }
            ext.modifiers = on(vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME);
            ext.foreign = on(vk::EXT_QUEUE_FAMILY_FOREIGN_NAME);
        }
        #[cfg(windows)]
        if on(vk::KHR_EXTERNAL_MEMORY_WIN32_NAME) {
            ext.win32 = Some(ash::khr::external_memory_win32::Device::new(
                &instance, &device,
            ));
        }
        #[cfg(target_vendor = "apple")]
        {
            ext.metal = on(vk::EXT_METAL_OBJECTS_NAME);
        }
        let _ = on;
        // SAFETY: plain queries and creations on the new device.
        let (mem, pool, sampler) = unsafe {
            let mem = instance.get_physical_device_memory_properties(physical);
            let pool = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .map_err(vk_error)?;
            let sampler = device
                .create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::LINEAR)
                        .min_filter(vk::Filter::LINEAR)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                    None,
                )
                .map_err(vk_error)?;
            (mem, pool, sampler)
        };
        let info = GpuInfo {
            name: props
                .device_name_as_c_str()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            kind: kind_of(props.device_type),
            api_version: (
                vk::api_version_major(props.api_version),
                vk::api_version_minor(props.api_version),
                vk::api_version_patch(props.api_version),
            ),
        };
        let dev = Dev {
            _entry: entry,
            instance,
            debug,
            _messages: kept_messages,
            physical,
            device,
            queue,
            family,
            ext,
            mem,
            pool,
            sampler,
            next_id: 0,
            res: HashMap::new(),
            batch: None,
            in_flight: VecDeque::new(),
            dead: Vec::new(),
            orphans: Vec::new(),
            next_seq: 1,
            completed: 0,
        };
        Ok((dev, info))
    }

    /// The best device: Vulkan 1.3, the features, a graphics and compute
    /// queue.
    fn pick(
        instance: &ash::Instance,
        prefer: Option<GpuKind>,
    ) -> Option<(vk::PhysicalDevice, u32, vk::PhysicalDeviceProperties)> {
        // SAFETY: plain queries on handles the instance returned.
        let devices = unsafe { instance.enumerate_physical_devices() }.ok()?;
        let mut best: Option<(u32, vk::PhysicalDevice, u32, vk::PhysicalDeviceProperties)> = None;
        for pd in devices {
            // SAFETY: as above.
            let (props, families, f13) = unsafe {
                let props = instance.get_physical_device_properties(pd);
                let families = instance.get_physical_device_queue_family_properties(pd);
                let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
                let mut f2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut f13);
                instance.get_physical_device_features2(pd, &mut f2);
                (
                    props,
                    families,
                    (f13.dynamic_rendering, f13.synchronization2),
                )
            };
            if props.api_version < vk::API_VERSION_1_3 || f13 != (vk::TRUE, vk::TRUE) {
                continue;
            }
            let want = vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE;
            let Some(family) = families.iter().position(|f| f.queue_flags.contains(want)) else {
                continue;
            };
            let r = rank(kind_of(props.device_type), prefer);
            if best.as_ref().is_none_or(|b| r < b.0) {
                best = Some((r, pd, family as u32, props));
            }
        }
        best.map(|(_, pd, family, props)| (pd, family, props))
    }

    fn memory_type(&self, bits: u32, want: vk::MemoryPropertyFlags) -> Option<u32> {
        (0..self.mem.memory_type_count).find(|&i| {
            bits & (1 << i) != 0
                && self.mem.memory_types[i as usize]
                    .property_flags
                    .contains(want)
        })
    }

    /// Allocates memory for `reqs`, preferring `prefer`, else any type
    /// with `need`.
    fn alloc(
        &self,
        reqs: vk::MemoryRequirements,
        prefer: vk::MemoryPropertyFlags,
        need: vk::MemoryPropertyFlags,
    ) -> Result<vk::DeviceMemory, GpuError> {
        let index = self
            .memory_type(reqs.memory_type_bits, prefer)
            .or_else(|| self.memory_type(reqs.memory_type_bits, need))
            .ok_or_else(|| GpuError::Unsupported("no suitable memory type".into()))?;
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(reqs.size)
            .memory_type_index(index);
        // SAFETY: a valid allocate info for this device.
        unsafe { self.device.allocate_memory(&info, None) }.map_err(vk_error)
    }

    fn raw_buffer(
        &self,
        size: u64,
        usage: vk::BufferUsageFlags,
        prefer: vk::MemoryPropertyFlags,
        need: vk::MemoryPropertyFlags,
    ) -> Result<(vk::Buffer, vk::DeviceMemory), GpuError> {
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: a valid create info; the buffer is destroyed if binding
        // its memory fails.
        unsafe {
            let buffer = self.device.create_buffer(&info, None).map_err(vk_error)?;
            let reqs = self.device.get_buffer_memory_requirements(buffer);
            let memory = match self.alloc(reqs, prefer, need) {
                Ok(m) => m,
                Err(e) => {
                    self.device.destroy_buffer(buffer, None);
                    return Err(e);
                }
            };
            if let Err(e) = self.device.bind_buffer_memory(buffer, memory, 0) {
                self.device.destroy_buffer(buffer, None);
                self.device.free_memory(memory, None);
                return Err(vk_error(e));
            }
            Ok((buffer, memory))
        }
    }

    /// A host-visible buffer holding `data`, or `len` zeros.
    fn staging(&self, len: u64, data: Option<&[u8]>) -> Result<Staging, GpuError> {
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let usage = vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST;
        let (buffer, memory) = self.raw_buffer(len.max(1), usage, host, host)?;
        let staging = Staging { buffer, memory };
        if let Some(data) = data {
            // SAFETY: the memory is host-visible and coherent, `len` bytes
            // long, and not in use by the GPU yet.
            unsafe {
                let ptr = self
                    .device
                    .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
                    .map_err(vk_error)?;
                core::ptr::copy_nonoverlapping(data.as_ptr(), ptr.cast::<u8>(), data.len());
                self.device.unmap_memory(memory);
            }
        }
        Ok(staging)
    }

    fn free_staging(&self, s: Staging) {
        // SAFETY: called only once the GPU is done with it.
        unsafe {
            self.device.destroy_buffer(s.buffer, None);
            self.device.free_memory(s.memory, None);
        }
    }

    fn destroy_kind(&self, kind: Kind) {
        destroy_kind(&self.device, kind);
    }
}

/// Destroys a resource's Vulkan objects.
fn destroy_kind(device: &ash::Device, kind: Kind) {
    // SAFETY: called only once no submitted work uses it.
    unsafe {
        match kind {
            Kind::Buffer { buffer, memory } => {
                device.destroy_buffer(buffer, None);
                device.free_memory(memory, None);
            }
            Kind::Image {
                image,
                memory,
                view,
                ..
            } => {
                device.destroy_image_view(view, None);
                device.destroy_image(image, None);
                device.free_memory(memory, None);
            }
            Kind::Program {
                pipeline,
                layout,
                set_layout,
            } => {
                device.destroy_pipeline(pipeline, None);
                device.destroy_pipeline_layout(layout, None);
                device.destroy_descriptor_set_layout(set_layout, None);
            }
        }
    }
}

impl Dev {
    fn add(&mut self, info: ResInfo, kind: Kind) -> u64 {
        self.next_id += 1;
        self.res.insert(
            self.next_id,
            Res {
                info,
                kind,
                track: Track::default(),
                last_use: 0,
            },
        );
        self.next_id
    }

    /// The batch being recorded, started if need be.
    fn batch(&mut self) -> Result<&mut Batch, GpuError> {
        if self.batch.is_none() {
            let info = vk::CommandBufferAllocateInfo::default()
                .command_pool(self.pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            // SAFETY: the pool is used only under the lock.
            let cmd = unsafe {
                let cmd = self
                    .device
                    .allocate_command_buffers(&info)
                    .map_err(vk_error)?[0];
                let begin = vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
                if let Err(e) = self.device.begin_command_buffer(cmd, &begin) {
                    self.device.free_command_buffers(self.pool, &[cmd]);
                    return Err(vk_error(e));
                }
                cmd
            };
            let seq = self.next_seq;
            self.next_seq += 1;
            self.batch = Some(Batch {
                seq,
                cmd,
                staging: Vec::new(),
                pools: Vec::new(),
            });
        }
        Ok(self.batch.as_mut().expect("just started"))
    }

    /// Records the barriers and layout transitions before `uses`.
    fn uses(&mut self, uses: &[Use]) -> Result<vk::CommandBuffer, GpuError> {
        let (seq, cmd) = {
            let b = self.batch()?;
            (b.seq, b.cmd)
        };
        // One use per resource: a resource bound twice is used once, with
        // both uses' stages and accesses.
        let mut merged: Vec<Use> = Vec::new();
        for u in uses {
            match merged.iter_mut().find(|m| m.id == u.id) {
                Some(m) => {
                    m.stage |= u.stage;
                    m.access |= u.access;
                    m.write |= u.write;
                    if let Some(layout) = u.layout {
                        // Two layouts at once means GENERAL.
                        m.layout = Some(match m.layout {
                            Some(prev) if prev != layout => vk::ImageLayout::GENERAL,
                            _ => layout,
                        });
                    }
                }
                None => merged.push(*u),
            }
        }
        let mut buffer_barriers = Vec::new();
        let mut image_barriers = Vec::new();
        let family = self.family;
        for u in &merged {
            let res = self.res.get_mut(&u.id).expect("validated");
            res.last_use = seq;
            let t = &mut res.track;
            let prior_stage = t.write.map_or(vk::PipelineStageFlags2::NONE, |w| w.0) | t.reads;
            let prior_access = t.write.map_or(vk::AccessFlags2::NONE, |w| w.1);
            match &mut res.kind {
                Kind::Image {
                    image,
                    layout,
                    foreign,
                    ..
                } => {
                    let want = u.layout.expect("an image use names its layout");
                    // An import's first use takes it over from its owner.
                    let (src_family, dst_family) = match *foreign {
                        Foreign::Acquire(from) => {
                            *foreign = Foreign::Held(from);
                            (from, family)
                        }
                        _ => (vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED),
                    };
                    let acquire = src_family != vk::QUEUE_FAMILY_IGNORED;
                    let transition = *layout != want;
                    let needed = acquire
                        || transition
                        || u.write && (t.write.is_some() || !t.reads.is_empty())
                        || !u.write && t.write.is_some() && !t.visible.contains(u.stage);
                    if needed {
                        image_barriers.push(
                            vk::ImageMemoryBarrier2::default()
                                .src_stage_mask(prior_stage)
                                .src_access_mask(prior_access)
                                .dst_stage_mask(u.stage)
                                .dst_access_mask(u.access)
                                .old_layout(*layout)
                                .new_layout(want)
                                .src_queue_family_index(src_family)
                                .dst_queue_family_index(dst_family)
                                .image(*image)
                                .subresource_range(COLOR),
                        );
                    }
                    *layout = want;
                    if u.write || transition {
                        // A layout transition writes the image.
                        t.write = Some((
                            u.stage,
                            if u.write {
                                u.access
                            } else {
                                vk::AccessFlags2::NONE
                            },
                        ));
                        t.reads = if u.write {
                            vk::PipelineStageFlags2::NONE
                        } else {
                            u.stage
                        };
                        t.visible = u.stage;
                    } else {
                        if needed {
                            t.visible |= u.stage;
                        }
                        t.reads |= u.stage;
                    }
                }
                Kind::Buffer { buffer, .. } => {
                    let needed = if u.write {
                        t.write.is_some() || !t.reads.is_empty()
                    } else {
                        t.write.is_some() && !t.visible.contains(u.stage)
                    };
                    if needed {
                        buffer_barriers.push(
                            vk::BufferMemoryBarrier2::default()
                                .src_stage_mask(prior_stage)
                                .src_access_mask(prior_access)
                                .dst_stage_mask(u.stage)
                                .dst_access_mask(u.access)
                                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .buffer(*buffer)
                                .offset(0)
                                .size(vk::WHOLE_SIZE),
                        );
                    }
                    if u.write {
                        t.write = Some((u.stage, u.access));
                        t.reads = vk::PipelineStageFlags2::NONE;
                        t.visible = vk::PipelineStageFlags2::NONE;
                    } else {
                        if needed {
                            t.visible |= u.stage;
                        }
                        t.reads |= u.stage;
                    }
                }
                Kind::Program { .. } => {}
            }
        }
        if !buffer_barriers.is_empty() || !image_barriers.is_empty() {
            let dep = vk::DependencyInfo::default()
                .buffer_memory_barriers(&buffer_barriers)
                .image_memory_barriers(&image_barriers);
            // SAFETY: `cmd` is recording, and the barriers name live
            // resources.
            unsafe { self.device.cmd_pipeline_barrier2(cmd, &dep) };
        }
        Ok(cmd)
    }

    /// Marks a program used by the batch, so it outlives the batch.
    fn use_program(
        &mut self,
        id: u64,
    ) -> Result<(vk::Pipeline, vk::PipelineLayout, vk::DescriptorSetLayout), GpuError> {
        let seq = self.batch()?.seq;
        let res = self.res.get_mut(&id).expect("validated");
        res.last_use = seq;
        match res.kind {
            Kind::Program {
                pipeline,
                layout,
                set_layout,
            } => Ok((pipeline, layout, set_layout)),
            _ => unreachable!("validated as a program"),
        }
    }

    /// A descriptor set with `bindings` written, from the batch's pools.
    fn descriptor_set(
        &mut self,
        set_layout: vk::DescriptorSetLayout,
        bindings: &[Bind],
    ) -> Result<vk::DescriptorSet, GpuError> {
        let layouts = [set_layout];
        let mut set = None;
        for attempt in 0..2 {
            let pool = match self.batch()?.pools.last() {
                Some(&p) if attempt == 0 => p,
                _ => {
                    let sizes = [
                        vk::DescriptorType::UNIFORM_BUFFER,
                        vk::DescriptorType::STORAGE_BUFFER,
                        vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                        vk::DescriptorType::STORAGE_IMAGE,
                    ]
                    .map(|ty| vk::DescriptorPoolSize {
                        ty,
                        descriptor_count: SETS_PER_POOL * 4,
                    });
                    let info = vk::DescriptorPoolCreateInfo::default()
                        .max_sets(SETS_PER_POOL)
                        .pool_sizes(&sizes);
                    // SAFETY: a valid create info.
                    let p = unsafe { self.device.create_descriptor_pool(&info, None) }
                        .map_err(vk_error)?;
                    self.batch()?.pools.push(p);
                    p
                }
            };
            let info = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pool)
                .set_layouts(&layouts);
            // SAFETY: the pool is used only under the lock.
            match unsafe { self.device.allocate_descriptor_sets(&info) } {
                Ok(sets) => {
                    set = Some(sets[0]);
                    break;
                }
                Err(vk::Result::ERROR_OUT_OF_POOL_MEMORY | vk::Result::ERROR_FRAGMENTED_POOL)
                    if attempt == 0 => {}
                Err(e) => return Err(vk_error(e)),
            }
        }
        let set = set.ok_or(GpuError::OutOfMemory)?;
        let mut buffer_infos = Vec::new();
        let mut image_infos = Vec::new();
        for b in bindings {
            match *b {
                Bind::Uniform(buf) | Bind::Storage(buf) => {
                    let Kind::Buffer { buffer, .. } = self.res[&buf.raw()].kind else {
                        unreachable!()
                    };
                    buffer_infos.push(vk::DescriptorBufferInfo {
                        buffer,
                        offset: 0,
                        range: vk::WHOLE_SIZE,
                    });
                }
                Bind::Sampled(img) | Bind::StorageImage(img) => {
                    let Kind::Image { view, .. } = self.res[&img.raw()].kind else {
                        unreachable!()
                    };
                    let layout = if matches!(b, Bind::Sampled(_)) {
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
                    } else {
                        vk::ImageLayout::GENERAL
                    };
                    image_infos.push(vk::DescriptorImageInfo {
                        sampler: self.sampler,
                        image_view: view,
                        image_layout: layout,
                    });
                }
            }
        }
        let (mut bi, mut ii) = (0, 0);
        let writes: Vec<vk::WriteDescriptorSet<'_>> = bindings
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let w = vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(i as u32)
                    .descriptor_type(descriptor_type(b.kind()));
                match b {
                    Bind::Uniform(_) | Bind::Storage(_) => {
                        bi += 1;
                        w.buffer_info(core::slice::from_ref(&buffer_infos[bi - 1]))
                    }
                    Bind::Sampled(_) | Bind::StorageImage(_) => {
                        ii += 1;
                        w.image_info(core::slice::from_ref(&image_infos[ii - 1]))
                    }
                }
            })
            .collect();
        // SAFETY: the set was just allocated and is not in use.
        unsafe { self.device.update_descriptor_sets(&writes, &[]) };
        Ok(set)
    }

    fn binding_uses(&self, bindings: &[Bind], compute: bool) -> Vec<Use> {
        let shader = if compute {
            vk::PipelineStageFlags2::COMPUTE_SHADER
        } else {
            vk::PipelineStageFlags2::VERTEX_SHADER | vk::PipelineStageFlags2::FRAGMENT_SHADER
        };
        let rw = vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE;
        bindings
            .iter()
            .map(|b| match *b {
                Bind::Uniform(x) => Use {
                    id: x.raw(),
                    stage: shader,
                    access: vk::AccessFlags2::UNIFORM_READ,
                    write: false,
                    layout: None,
                },
                Bind::Storage(x) => Use {
                    id: x.raw(),
                    stage: shader,
                    access: rw,
                    write: true,
                    layout: None,
                },
                Bind::Sampled(x) => Use {
                    id: x.raw(),
                    stage: shader,
                    access: vk::AccessFlags2::SHADER_SAMPLED_READ,
                    write: false,
                    layout: Some(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
                },
                Bind::StorageImage(x) => Use {
                    id: x.raw(),
                    stage: shader,
                    access: rw,
                    write: true,
                    layout: Some(vk::ImageLayout::GENERAL),
                },
            })
            .collect()
    }

    /// Submits the batch, if any. Its sequence number and fence.
    fn submit(&mut self) -> Result<Option<(u64, vk::Fence)>, GpuError> {
        let Some(batch) = self.batch.take() else {
            return Ok(None);
        };
        // SAFETY: the command buffer is recording; the fence is new; the
        // queue is used only under the lock.
        let fence = unsafe {
            if let Err(e) = self.device.end_command_buffer(batch.cmd) {
                return Err(vk_error(e));
            }
            let fence = self
                .device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(vk_error)?;
            let cmds = [batch.cmd];
            let submit = vk::SubmitInfo::default().command_buffers(&cmds);
            if let Err(e) = self.device.queue_submit(self.queue, &[submit], fence) {
                self.device.destroy_fence(fence, None);
                return Err(vk_error(e));
            }
            fence
        };
        self.in_flight.push_back(InFlight {
            seq: batch.seq,
            cmd: batch.cmd,
            fence,
            staging: batch.staging,
            pools: batch.pools,
            holds: 0,
            signal: None,
        });
        Ok(Some((batch.seq, fence)))
    }

    fn in_flight(&mut self, seq: u64) -> Option<&mut InFlight> {
        self.in_flight.iter_mut().find(|f| f.seq == seq)
    }

    /// Frees what finished batches held, and destroys the resources only
    /// they still used.
    fn retire(&mut self) {
        while let Some(front) = self.in_flight.front() {
            // SAFETY: the fence is alive: it is destroyed only here.
            let signaled = unsafe { self.device.get_fence_status(front.fence) }.unwrap_or(false);
            let waited = front.signal.as_ref().is_none_or(|s| s.is_done());
            if !signaled || front.holds > 0 || !waited {
                break;
            }
            let done = self.in_flight.pop_front().expect("checked");
            // SAFETY: the GPU has finished the batch and nobody waits on
            // its fence.
            unsafe {
                self.device.destroy_fence(done.fence, None);
                self.device.free_command_buffers(self.pool, &[done.cmd]);
                for p in &done.pools {
                    self.device.destroy_descriptor_pool(*p, None);
                }
            }
            for s in done.staging {
                self.free_staging(s);
            }
            self.completed = done.seq;
        }
        let completed = self.completed;
        let (ready, waiting): (Vec<_>, Vec<_>) = core::mem::take(&mut self.dead)
            .into_iter()
            .partition(|(seq, _)| *seq <= completed);
        self.dead = waiting;
        for (_, kind) in ready {
            self.destroy_kind(kind);
        }
        let (ready, waiting): (Vec<_>, Vec<_>) = core::mem::take(&mut self.orphans)
            .into_iter()
            .partition(|(seq, _)| *seq <= completed);
        self.orphans = waiting;
        for (_, staging) in ready {
            self.free_staging(staging);
        }
    }

    fn destroy(&mut self, id: u64) {
        let Some(res) = self.res.remove(&id) else {
            return;
        };
        if res.last_use <= self.completed {
            self.destroy_kind(res.kind);
        } else {
            self.dead.push((res.last_use, res.kind));
        }
        self.retire();
    }

    // The operations, as plain sync code.

    fn create_buffer(&mut self, op: CreateBuffer) -> Result<Buffer, GpuError> {
        validate::create_buffer(&op)?;
        self.retire();
        let mut usage = vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST;
        for (ours, theirs) in [
            (BufferUsage::VERTEX, vk::BufferUsageFlags::VERTEX_BUFFER),
            (BufferUsage::UNIFORM, vk::BufferUsageFlags::UNIFORM_BUFFER),
            (BufferUsage::STORAGE, vk::BufferUsageFlags::STORAGE_BUFFER),
        ] {
            if op.usage.contains(ours) {
                usage |= theirs;
            }
        }
        let (buffer, memory) = self.raw_buffer(
            op.size,
            usage,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
            vk::MemoryPropertyFlags::empty(),
        )?;
        let id = self.add(
            ResInfo::Buffer {
                size: op.size,
                usage: op.usage,
            },
            Kind::Buffer { buffer, memory },
        );
        Ok(Buffer(id))
    }

    fn create_image(&mut self, op: CreateImage) -> Result<Image, GpuError> {
        self.create_image_as(op, false)
    }

    /// A view of all of `image`; `opaque` reads alpha as 1 (for formats
    /// with an unused alpha), where the view is only sampled.
    fn view(
        &self,
        image: vk::Image,
        format: vk::Format,
        usage: ImageUsage,
        opaque: bool,
    ) -> Result<vk::ImageView, GpuError> {
        let mut components = vk::ComponentMapping::default();
        if opaque && usage == ImageUsage::SAMPLED {
            components.a = vk::ComponentSwizzle::ONE;
        }
        let info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .components(components)
            .subresource_range(COLOR);
        // SAFETY: a live image, created with this format.
        unsafe { self.device.create_image_view(&info, None) }.map_err(vk_error)
    }

    /// The image usage and format features for `usage`.
    fn image_usage(usage: ImageUsage) -> (vk::ImageUsageFlags, vk::FormatFeatureFlags) {
        let mut flags = vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST;
        let mut features =
            vk::FormatFeatureFlags::TRANSFER_SRC | vk::FormatFeatureFlags::TRANSFER_DST;
        for (ours, theirs, feature) in [
            (
                ImageUsage::SAMPLED,
                vk::ImageUsageFlags::SAMPLED,
                vk::FormatFeatureFlags::SAMPLED_IMAGE,
            ),
            (
                ImageUsage::TARGET,
                vk::ImageUsageFlags::COLOR_ATTACHMENT,
                vk::FormatFeatureFlags::COLOR_ATTACHMENT,
            ),
            (
                ImageUsage::STORAGE,
                vk::ImageUsageFlags::STORAGE,
                vk::FormatFeatureFlags::STORAGE_IMAGE,
            ),
        ] {
            if usage.contains(ours) {
                flags |= theirs;
                features |= feature;
            }
        }
        (flags, features)
    }

    fn create_image_as(&mut self, op: CreateImage, opaque: bool) -> Result<Image, GpuError> {
        validate::create_image(&op)?;
        self.retire();
        let format = vk_format(op.format);
        let (usage, features) = Self::image_usage(op.usage);
        // SAFETY: a plain query.
        let props = unsafe {
            self.instance
                .get_physical_device_format_properties(self.physical, format)
        };
        if !props.optimal_tiling_features.contains(features) {
            return Err(GpuError::Unsupported(format!(
                "{:?} images cannot be used for {:?} on this device",
                op.format, op.usage
            )));
        }
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: op.width,
                height: op.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: valid create infos; everything made is destroyed on a
        // later failure.
        let (image, memory, view) = unsafe {
            let image = self.device.create_image(&info, None).map_err(vk_error)?;
            let reqs = self.device.get_image_memory_requirements(image);
            let memory = match self.alloc(
                reqs,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
                vk::MemoryPropertyFlags::empty(),
            ) {
                Ok(m) => m,
                Err(e) => {
                    self.device.destroy_image(image, None);
                    return Err(e);
                }
            };
            if let Err(e) = self.device.bind_image_memory(image, memory, 0) {
                self.device.destroy_image(image, None);
                self.device.free_memory(memory, None);
                return Err(vk_error(e));
            }
            match self.view(image, format, op.usage, opaque) {
                Ok(v) => (image, memory, v),
                Err(e) => {
                    self.device.destroy_image(image, None);
                    self.device.free_memory(memory, None);
                    return Err(e);
                }
            }
        };
        let id = self.add(
            ResInfo::Image {
                width: op.width,
                height: op.height,
                format: op.format,
                usage: op.usage,
            },
            Kind::Image {
                image,
                memory,
                view,
                layout: vk::ImageLayout::UNDEFINED,
                foreign: Foreign::No,
            },
        );
        Ok(Image(id))
    }

    fn shader(
        device: &ash::Device,
        spirv: &[u32],
        label: &str,
    ) -> Result<vk::ShaderModule, GpuError> {
        if spirv.is_empty() {
            return Err(crate::invalid(format!("program `{label}` has no SPIR-V")));
        }
        let info = vk::ShaderModuleCreateInfo::default().code(spirv);
        // SAFETY: the code is the caller's SPIR-V; drivers reject invalid
        // modules, and validation catches the rest.
        unsafe { device.create_shader_module(&info, None) }.map_err(vk_error)
    }

    /// Builds a program's Vulkan objects: no lock needed, since it touches
    /// only the device, which Vulkan lets any thread use.
    fn build_program(
        device: &ash::Device,
        op: &CreateProgram<'_>,
    ) -> Result<(ResInfo, Kind), GpuError> {
        validate::create_program(op)?;
        let set_bindings: Vec<vk::DescriptorSetLayoutBinding<'_>> = op
            .bindings
            .iter()
            .enumerate()
            .map(|(i, k)| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(i as u32)
                    .descriptor_type(descriptor_type(*k))
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::ALL)
            })
            .collect();
        let ranges = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::ALL,
            offset: 0,
            size: op.push_constants,
        }];
        let ranges: &[vk::PushConstantRange] = if op.push_constants > 0 { &ranges } else { &[] };
        // SAFETY: valid create infos; everything is destroyed on failure.
        let (set_layout, layout) = unsafe {
            let set_layout = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&set_bindings),
                    None,
                )
                .map_err(vk_error)?;
            let layouts = [set_layout];
            let info = vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&layouts)
                .push_constant_ranges(ranges);
            match device.create_pipeline_layout(&info, None) {
                Ok(l) => (set_layout, l),
                Err(e) => {
                    device.destroy_descriptor_set_layout(set_layout, None);
                    return Err(vk_error(e));
                }
            }
        };
        let built = Self::pipeline(device, op, layout);
        let (pipeline, compute, target, stride) = match built {
            Ok(p) => p,
            Err(e) => {
                // SAFETY: nothing uses them yet.
                unsafe {
                    device.destroy_pipeline_layout(layout, None);
                    device.destroy_descriptor_set_layout(set_layout, None);
                }
                return Err(e);
            }
        };
        Ok((
            ResInfo::Program {
                label: op.label.into(),
                compute,
                bindings: op.bindings.to_vec(),
                push_constants: op.push_constants,
                target,
                stride,
            },
            Kind::Program {
                pipeline,
                layout,
                set_layout,
            },
        ))
    }

    fn pipeline(
        device: &ash::Device,
        op: &CreateProgram<'_>,
        layout: vk::PipelineLayout,
    ) -> Result<(vk::Pipeline, bool, Option<Format>, u32), GpuError> {
        let bad_entry = || {
            crate::invalid(format!(
                "program `{}`: an entry point holds a NUL byte",
                op.label
            ))
        };
        match op.kind {
            ProgramKind::Compute { spirv, entry } => {
                let module = Self::shader(device, spirv, op.label)?;
                let name = CString::new(entry).map_err(|_| bad_entry())?;
                let stage = vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::COMPUTE)
                    .module(module)
                    .name(&name);
                let info = vk::ComputePipelineCreateInfo::default()
                    .stage(stage)
                    .layout(layout);
                // SAFETY: valid create info; the module is destroyed after.
                let made = unsafe {
                    let made =
                        device.create_compute_pipelines(vk::PipelineCache::null(), &[info], None);
                    device.destroy_shader_module(module, None);
                    made
                };
                let pipeline = made.map_err(|(_, e)| vk_error(e))?[0];
                Ok((pipeline, true, None, 0))
            }
            ProgramKind::Draw {
                vertex,
                vertex_entry,
                fragment,
                fragment_entry,
                attrs,
                stride,
                target,
                blend,
            } => {
                let vs = Self::shader(device, vertex, op.label)?;
                let fs = match Self::shader(device, fragment, op.label) {
                    Ok(m) => m,
                    Err(e) => {
                        // SAFETY: unused.
                        unsafe { device.destroy_shader_module(vs, None) };
                        return Err(e);
                    }
                };
                let destroy_modules = || {
                    // SAFETY: the pipeline (if any) no longer needs them.
                    unsafe {
                        device.destroy_shader_module(vs, None);
                        device.destroy_shader_module(fs, None);
                    }
                };
                let (Ok(vname), Ok(fname)) =
                    (CString::new(vertex_entry), CString::new(fragment_entry))
                else {
                    destroy_modules();
                    return Err(bad_entry());
                };
                let stages = [
                    vk::PipelineShaderStageCreateInfo::default()
                        .stage(vk::ShaderStageFlags::VERTEX)
                        .module(vs)
                        .name(&vname),
                    vk::PipelineShaderStageCreateInfo::default()
                        .stage(vk::ShaderStageFlags::FRAGMENT)
                        .module(fs)
                        .name(&fname),
                ];
                let vbind = [vk::VertexInputBindingDescription {
                    binding: 0,
                    stride,
                    input_rate: vk::VertexInputRate::VERTEX,
                }];
                let vattrs: Vec<vk::VertexInputAttributeDescription> = attrs
                    .iter()
                    .enumerate()
                    .map(|(i, a)| vk::VertexInputAttributeDescription {
                        location: i as u32,
                        binding: 0,
                        format: attr_format(a.format),
                        offset: a.offset,
                    })
                    .collect();
                let vbind: &[vk::VertexInputBindingDescription] =
                    if stride > 0 { &vbind } else { &[] };
                let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
                    .vertex_binding_descriptions(vbind)
                    .vertex_attribute_descriptions(&vattrs);
                let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
                    .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
                let viewport = vk::PipelineViewportStateCreateInfo::default()
                    .viewport_count(1)
                    .scissor_count(1);
                let raster = vk::PipelineRasterizationStateCreateInfo::default()
                    .polygon_mode(vk::PolygonMode::FILL)
                    .cull_mode(vk::CullModeFlags::NONE)
                    .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
                    .line_width(1.0);
                let multisample = vk::PipelineMultisampleStateCreateInfo::default()
                    .rasterization_samples(vk::SampleCountFlags::TYPE_1);
                let mut attachment = vk::PipelineColorBlendAttachmentState::default()
                    .color_write_mask(vk::ColorComponentFlags::RGBA);
                if blend == Blend::PremultipliedAlpha {
                    attachment = attachment
                        .blend_enable(true)
                        .src_color_blend_factor(vk::BlendFactor::ONE)
                        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                        .color_blend_op(vk::BlendOp::ADD)
                        .src_alpha_blend_factor(vk::BlendFactor::ONE)
                        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                        .alpha_blend_op(vk::BlendOp::ADD);
                }
                let attachments = [attachment];
                let color_blend =
                    vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
                let dynamics = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
                let dynamic =
                    vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamics);
                let formats = [vk_format(target)];
                let mut rendering =
                    vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
                let info = vk::GraphicsPipelineCreateInfo::default()
                    .push_next(&mut rendering)
                    .stages(&stages)
                    .vertex_input_state(&vertex_input)
                    .input_assembly_state(&assembly)
                    .viewport_state(&viewport)
                    .rasterization_state(&raster)
                    .multisample_state(&multisample)
                    .color_blend_state(&color_blend)
                    .dynamic_state(&dynamic)
                    .layout(layout);
                // SAFETY: a valid create info, everything it points to
                // alive for the call.
                let made = unsafe {
                    device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
                };
                destroy_modules();
                let pipeline = made.map_err(|(_, e)| vk_error(e))?[0];
                Ok((pipeline, false, Some(target), stride))
            }
        }
    }

    fn add_program(&mut self, (info, kind): (ResInfo, Kind)) -> Program {
        self.retire();
        Program(self.add(info, kind))
    }

    fn upload(&mut self, op: Upload<'_>) -> Result<(), GpuError> {
        validate::upload(&*self, op.buffer.raw(), op.offset, op.data.len() as u64)?;
        if op.data.is_empty() {
            return Ok(());
        }
        self.retire();
        let staging = self.staging(op.data.len() as u64, Some(op.data))?;
        let cmd = self.uses(&[Use {
            id: op.buffer.raw(),
            stage: vk::PipelineStageFlags2::COPY,
            access: vk::AccessFlags2::TRANSFER_WRITE,
            write: true,
            layout: None,
        }])?;
        let Kind::Buffer { buffer, .. } = self.res[&op.buffer.raw()].kind else {
            unreachable!()
        };
        let region = vk::BufferCopy {
            src_offset: 0,
            dst_offset: op.offset,
            size: op.data.len() as u64,
        };
        // SAFETY: both buffers are alive and the region is in range.
        unsafe {
            self.device
                .cmd_copy_buffer(cmd, staging.buffer, buffer, &[region])
        };
        self.batch()?.staging.push(staging);
        Ok(())
    }

    fn image_region(&self, id: u64) -> (vk::Image, vk::BufferImageCopy) {
        let res = &self.res[&id];
        let (Kind::Image { image, .. }, ResInfo::Image { width, height, .. }) =
            (&res.kind, &res.info)
        else {
            unreachable!()
        };
        let region = vk::BufferImageCopy {
            buffer_offset: 0,
            buffer_row_length: 0,
            buffer_image_height: 0,
            image_subresource: COLOR_LAYER,
            image_offset: vk::Offset3D::default(),
            image_extent: vk::Extent3D {
                width: *width,
                height: *height,
                depth: 1,
            },
        };
        (*image, region)
    }

    fn upload_image(&mut self, op: UploadImage<'_>) -> Result<(), GpuError> {
        validate::whole_image(&*self, op.image.raw(), op.data.len(), "upload")?;
        self.retire();
        let staging = self.staging(op.data.len() as u64, Some(op.data))?;
        let cmd = self.uses(&[Use {
            id: op.image.raw(),
            stage: vk::PipelineStageFlags2::COPY,
            access: vk::AccessFlags2::TRANSFER_WRITE,
            write: true,
            layout: Some(vk::ImageLayout::TRANSFER_DST_OPTIMAL),
        }])?;
        let (image, region) = self.image_region(op.image.raw());
        // SAFETY: the image is in TRANSFER_DST_OPTIMAL and the staging
        // buffer holds the whole image.
        unsafe {
            self.device.cmd_copy_buffer_to_image(
                cmd,
                staging.buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );
        }
        self.batch()?.staging.push(staging);
        Ok(())
    }

    fn copy(&mut self, op: CopyBuffer) -> Result<(), GpuError> {
        validate::copy(&*self, &op)?;
        if op.size == 0 {
            return Ok(());
        }
        self.retire();
        let uses = [
            Use {
                id: op.src.raw(),
                stage: vk::PipelineStageFlags2::COPY,
                access: vk::AccessFlags2::TRANSFER_READ,
                write: false,
                layout: None,
            },
            Use {
                id: op.dst.raw(),
                stage: vk::PipelineStageFlags2::COPY,
                access: vk::AccessFlags2::TRANSFER_WRITE,
                write: true,
                layout: None,
            },
        ];
        let cmd = self.uses(&uses)?;
        let Kind::Buffer { buffer: src, .. } = self.res[&op.src.raw()].kind else {
            unreachable!()
        };
        let Kind::Buffer { buffer: dst, .. } = self.res[&op.dst.raw()].kind else {
            unreachable!()
        };
        let region = vk::BufferCopy {
            src_offset: op.src_offset,
            dst_offset: op.dst_offset,
            size: op.size,
        };
        // SAFETY: validated ranges on live buffers.
        unsafe { self.device.cmd_copy_buffer(cmd, src, dst, &[region]) };
        Ok(())
    }

    fn dispatch(&mut self, op: Dispatch<'_>) -> Result<(), GpuError> {
        validate::dispatch(&*self, &op)?;
        self.retire();
        let uses = self.binding_uses(op.bindings, true);
        let cmd = self.uses(&uses)?;
        let (pipeline, layout, set_layout) = self.use_program(op.program.raw())?;
        let set = if op.bindings.is_empty() {
            None
        } else {
            Some(self.descriptor_set(set_layout, op.bindings)?)
        };
        // SAFETY: a compute pipeline with its layout, a set matching it,
        // and push constants of the declared size.
        unsafe {
            self.device
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline);
            if let Some(set) = set {
                self.device.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &[set],
                    &[],
                );
            }
            if !op.push.is_empty() {
                self.device
                    .cmd_push_constants(cmd, layout, vk::ShaderStageFlags::ALL, 0, op.push);
            }
            let [x, y, z] = op.groups;
            self.device.cmd_dispatch(cmd, x, y, z);
        }
        Ok(())
    }

    fn draw(&mut self, op: Draw<'_>) -> Result<(), GpuError> {
        validate::draw(&*self, &op)?;
        self.retire();
        let mut uses = self.binding_uses(op.bindings, false);
        uses.push(Use {
            id: op.target.raw(),
            stage: vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            access: vk::AccessFlags2::COLOR_ATTACHMENT_WRITE
                | vk::AccessFlags2::COLOR_ATTACHMENT_READ,
            write: true,
            layout: Some(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL),
        });
        if let Some(v) = op.vertices {
            uses.push(Use {
                id: v.raw(),
                stage: vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
                access: vk::AccessFlags2::VERTEX_ATTRIBUTE_READ,
                write: false,
                layout: None,
            });
        }
        let cmd = self.uses(&uses)?;
        let (pipeline, layout, set_layout) = self.use_program(op.program.raw())?;
        let set = if op.bindings.is_empty() {
            None
        } else {
            Some(self.descriptor_set(set_layout, op.bindings)?)
        };
        let target = &self.res[&op.target.raw()];
        let (Kind::Image { view, .. }, ResInfo::Image { width, height, .. }) =
            (&target.kind, &target.info)
        else {
            unreachable!()
        };
        let (view, width, height) = (*view, *width, *height);
        let vertex_buffer = op.vertices.map(|v| {
            let Kind::Buffer { buffer, .. } = self.res[&v.raw()].kind else {
                unreachable!()
            };
            buffer
        });
        let clear = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: op.clear.unwrap_or([0.0; 4]),
            },
        };
        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(if op.clear.is_some() {
                vk::AttachmentLoadOp::CLEAR
            } else {
                vk::AttachmentLoadOp::LOAD
            })
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(clear)];
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D { width, height },
        };
        let rendering = vk::RenderingInfo::default()
            .render_area(area)
            .layer_count(1)
            .color_attachments(&attachment);
        // SAFETY: the target is in COLOR_ATTACHMENT_OPTIMAL in the
        // pipeline's format; the set and push constants match its layout.
        unsafe {
            self.device.cmd_begin_rendering(cmd, &rendering);
            self.device
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
            self.device.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: width as f32,
                    height: height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            self.device.cmd_set_scissor(cmd, 0, &[area]);
            if let Some(set) = set {
                self.device.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    layout,
                    0,
                    &[set],
                    &[],
                );
            }
            if !op.push.is_empty() {
                self.device
                    .cmd_push_constants(cmd, layout, vk::ShaderStageFlags::ALL, 0, op.push);
            }
            if let Some(buffer) = vertex_buffer {
                self.device.cmd_bind_vertex_buffers(cmd, 0, &[buffer], &[0]);
            }
            if op.vertex_count > 0 && op.instance_count > 0 {
                self.device
                    .cmd_draw(cmd, op.vertex_count, op.instance_count, 0, 0);
            }
            self.device.cmd_end_rendering(cmd);
        }
        Ok(())
    }

    /// Records a copy into a staging buffer and submits: the first half of
    /// a download.
    fn start_download(
        &mut self,
        src: u64,
        offset: u64,
        len: u64,
        image: bool,
    ) -> Result<Pending, GpuError> {
        self.retire();
        let staging = self.staging(len, None)?;
        let cmd = self.uses(&[Use {
            id: src,
            stage: vk::PipelineStageFlags2::COPY,
            access: vk::AccessFlags2::TRANSFER_READ,
            write: false,
            layout: image.then_some(vk::ImageLayout::TRANSFER_SRC_OPTIMAL),
        }])?;
        // SAFETY: validated ranges on live resources; the barrier makes the
        // copy visible to the host once the fence signals.
        unsafe {
            if image {
                let (img, region) = self.image_region(src);
                self.device.cmd_copy_image_to_buffer(
                    cmd,
                    img,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    staging.buffer,
                    &[region],
                );
            } else {
                let Kind::Buffer { buffer, .. } = self.res[&src].kind else {
                    unreachable!()
                };
                let region = vk::BufferCopy {
                    src_offset: offset,
                    dst_offset: 0,
                    size: len,
                };
                self.device
                    .cmd_copy_buffer(cmd, buffer, staging.buffer, &[region]);
            }
            let to_host = [vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ)];
            self.device.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().memory_barriers(&to_host),
            );
        }
        let (seq, fence) = self.submit()?.expect("the copy was just recorded");
        self.hold(seq, 1);
        Ok(Pending {
            seq,
            fence,
            staging: Some(staging),
        })
    }

    /// Submits everything, for `Finish`: the last batch's fence to wait on
    /// (submissions finish in order), if anything is in flight.
    fn start_finish(&mut self) -> Result<Option<Pending>, GpuError> {
        self.submit()?;
        self.retire();
        let last = self.in_flight.back_mut().map(|f| {
            f.holds += 1;
            Pending {
                seq: f.seq,
                fence: f.fence,
                staging: None,
            }
        });
        Ok(last)
    }

    /// The second half of a wait: reads the staging buffer into `out`.
    fn end_wait(&mut self, mut pending: Pending, out: Option<&mut [u8]>) -> Result<(), GpuError> {
        self.hold(pending.seq, -1);
        let staging = pending.staging.take();
        let result = match (staging, out) {
            (Some(staging), Some(out)) => {
                // SAFETY: the fence has signalled, and the barrier made the
                // copy visible to the host; the memory is coherent.
                let read = unsafe {
                    self.device
                        .map_memory(
                            staging.memory,
                            0,
                            vk::WHOLE_SIZE,
                            vk::MemoryMapFlags::empty(),
                        )
                        .map(|ptr| {
                            core::ptr::copy_nonoverlapping(
                                ptr.cast::<u8>(),
                                out.as_mut_ptr(),
                                out.len(),
                            );
                            self.device.unmap_memory(staging.memory);
                        })
                        .map_err(vk_error)
                };
                self.free_staging(staging);
                read
            }
            (Some(staging), None) => {
                self.free_staging(staging);
                Ok(())
            }
            _ => Ok(()),
        };
        self.retire();
        result
    }

    fn hold(&mut self, seq: u64, by: i32) {
        if let Some(f) = self.in_flight(seq) {
            f.holds = (f.holds as i64 + i64::from(by)).max(0) as usize;
        }
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        // SAFETY: after waiting for the device to be idle, nothing is in
        // use; everything is destroyed child first.
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Some(batch) = self.batch.take() {
                self.device.free_command_buffers(self.pool, &[batch.cmd]);
                for p in batch.pools {
                    self.device.destroy_descriptor_pool(p, None);
                }
                for s in batch.staging {
                    self.free_staging(s);
                }
            }
            for f in core::mem::take(&mut self.in_flight) {
                self.device.destroy_fence(f.fence, None);
                self.device.free_command_buffers(self.pool, &[f.cmd]);
                for p in f.pools {
                    self.device.destroy_descriptor_pool(p, None);
                }
                for s in f.staging {
                    self.free_staging(s);
                }
            }
            for (_, kind) in core::mem::take(&mut self.dead) {
                self.destroy_kind(kind);
            }
            for (_, staging) in core::mem::take(&mut self.orphans) {
                self.free_staging(staging);
            }
            for (_, res) in core::mem::take(&mut self.res) {
                self.destroy_kind(res.kind);
            }
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_command_pool(self.pool, None);
            self.device.destroy_device(None);
            if let Some((utils, m)) = self.debug.take() {
                utils.destroy_debug_utils_messenger(m, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // Stop the waiter first: it may be waiting on one of the device's
        // fences.
        if let Some(waiter) = self.waiter.get() {
            drop(
                waiter
                    .jobs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take(),
            );
            if let Some(thread) = waiter
                .thread
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                let _ = thread.join();
            }
        }
    }
}

impl Ash {
    /// The best GPU, with the default [`AshOptions`].
    pub fn new() -> Result<Self, GpuError> {
        Self::with(&AshOptions::default())
    }

    /// A GPU picked and set up by `options`.
    pub fn with(options: &AshOptions) -> Result<Self, GpuError> {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let (dev, info) = Dev::open(options, &messages)?;
        let device = dev.device.clone();
        Ok(Self {
            shared: Arc::new(Shared {
                dev: Mutex::new(dev),
                device,
                info,
                messages,
                waiter: OnceLock::new(),
                spin: options.spin,
                waiter_stack: options.waiter_stack,
            }),
        })
    }

    /// What the validation layer reported since the last call (with
    /// [`AshOptions::validation`]), emptying it.
    pub fn validation_messages(&self) -> Vec<String> {
        core::mem::take(
            &mut *self
                .shared
                .messages
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// How many batches are submitted and not yet cleaned up, and how many
    /// staging buffers of cancelled downloads wait to be freed: for tests.
    #[doc(hidden)]
    pub fn pending_counts(&self) -> (usize, usize) {
        let mut dev = self.dev();
        dev.retire();
        (dev.in_flight.len(), dev.orphans.len())
    }

    fn dev(&self) -> MutexGuard<'_, Dev> {
        self.shared
            .dev
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `fence` has signalled. It must be held.
    fn signaled(&self, fence: vk::Fence) -> Result<bool, GpuError> {
        // SAFETY: the caller holds the fence alive.
        unsafe { self.shared.device.get_fence_status(fence) }.map_err(vk_error)
    }

    /// Checks `fence`, then keeps checking for up to the spin budget.
    /// Whether it signalled. It must be held.
    fn spin(&self, fence: vk::Fence) -> Result<bool, GpuError> {
        if self.signaled(fence)? {
            return Ok(true);
        }
        let budget = self.shared.spin;
        if budget.is_zero() {
            return Ok(false);
        }
        let until = std::time::Instant::now() + budget;
        while std::time::Instant::now() < until {
            core::hint::spin_loop();
            if self.signaled(fence)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Waits for `fence` on this thread: spins, then sleeps on it. The
    /// blocking form. Its batch must be held.
    fn block(&self, fence: vk::Fence) -> Result<(), GpuError> {
        if self.spin(fence)? {
            return Ok(());
        }
        // SAFETY: held by the caller.
        unsafe {
            self.shared
                .device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(vk_error)
        }
    }

    /// Waits for batch `seq` without blocking: spins, then hands the fence
    /// to the waiter thread (once per batch) and awaits it. The async form.
    /// The batch must be held.
    async fn wait(&self, seq: u64, fence: vk::Fence) -> Result<(), GpuError> {
        if self.spin(fence)? {
            return Ok(());
        }
        let (signal, send) = {
            let mut dev = self.dev();
            // Held, so it is still in flight.
            let Some(f) = dev.in_flight(seq) else {
                return Ok(());
            };
            match &f.signal {
                Some(s) => (Arc::clone(s), false),
                None => {
                    let s = Arc::new(Signal::default());
                    f.signal = Some(Arc::clone(&s));
                    (s, true)
                }
            }
        };
        if send {
            self.send(Job {
                fence,
                signal: Arc::clone(&signal),
            });
        }
        FenceWait(signal).await
    }

    /// Gives the waiter thread a fence, starting the thread if need be.
    fn send(&self, job: Job) {
        let waiter = self.shared.waiter.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            let device = self.shared.device.clone();
            let thread = std::thread::Builder::new()
                .name("seam-ash fences".to_string())
                .stack_size(self.shared.waiter_stack)
                .spawn(move || {
                    for job in rx {
                        // SAFETY: the fence stays alive until its signal is
                        // done.
                        let waited =
                            unsafe { device.wait_for_fences(&[job.fence], true, u64::MAX) };
                        job.signal.complete(waited.map_err(vk_error));
                    }
                });
            let (jobs, thread) = match thread {
                Ok(t) => (Some(tx), Some(t)),
                Err(_) => (None, None),
            };
            Waiter {
                jobs: Mutex::new(jobs),
                thread: Mutex::new(thread),
            }
        });
        let signal = Arc::clone(&job.signal);
        let sent = waiter
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|tx| tx.send(job).is_ok());
        if !sent {
            signal.complete(Err(GpuError::Unsupported(
                "the fence waiter thread could not start".into(),
            )));
        }
    }
}

/// A wait in progress: lets go of its batch when done or dropped. If the
/// future waiting on a download is dropped, the staging buffer goes to the
/// device's orphans, freed once its batch is.
struct InProgress<'a> {
    ash: &'a Ash,
    pending: Option<Pending>,
}

impl InProgress<'_> {
    fn finish(mut self, out: Option<&mut [u8]>) -> Result<(), GpuError> {
        let pending = self.pending.take().expect("finished once");
        self.ash.dev().end_wait(pending, out)
    }
}

impl Drop for InProgress<'_> {
    fn drop(&mut self) {
        if let Some(mut pending) = self.pending.take() {
            let mut dev = self.ash.dev();
            dev.hold(pending.seq, -1);
            if let Some(staging) = pending.staging.take() {
                dev.orphans.push((pending.seq, staging));
            }
            dev.retire();
        }
    }
}

/// Implements an operation that never waits, in every form, from one sync
/// body run under the lock.
macro_rules! now {
    ($([$($l:lifetime),*] $op:ty => $out:ty, |$dev:ident, $o:pat_param| $body:expr;)*) => {$(
        impl<$($l),*> Perform<$op> for Ash {
            fn perform(&mut self, $o: $op) -> $out {
                let $dev = &mut *self.dev();
                $body
            }
        }

        impl<$($l),*> seam_core::future::AsyncPerform<$op> for Ash {
            async fn perform_async(&mut self, $o: $op) -> $out {
                let $dev = &mut *self.dev();
                $body
            }
        }

        impl<$($l),*> seam_core::future::SendPerform<$op> for Ash {
            async fn perform_send(&mut self, $o: $op) -> $out {
                let $dev = &mut *self.dev();
                $body
            }
        }
    )*};
}

now! {
    [] Opening<CreateBuffer> => Result<Buffer, GpuError>, |dev, Opening(op, _)| dev.create_buffer(op);
    [] Closing<CreateBuffer> => Result<(), GpuError>, |dev, Closing(b, _)| { dev.destroy(b.raw()); Ok(()) };
    [] Opening<CreateImage> => Result<Image, GpuError>, |dev, Opening(op, _)| dev.create_image(op);
    [] Closing<CreateImage> => Result<(), GpuError>, |dev, Closing(i, _)| { dev.destroy(i.raw()); Ok(()) };
    ['a] Closing<CreateProgram<'a>> => Result<(), GpuError>, |dev, Closing(p, _)| { dev.destroy(p.raw()); Ok(()) };
    ['a] Upload<'a> => Result<(), GpuError>, |dev, op| dev.upload(op);
    ['a] UploadImage<'a> => Result<(), GpuError>, |dev, op| dev.upload_image(op);
    [] CopyBuffer => Result<(), GpuError>, |dev, op| dev.copy(op);
    ['a] Dispatch<'a> => Result<(), GpuError>, |dev, op| dev.dispatch(op);
    ['a] Draw<'a> => Result<(), GpuError>, |dev, op| dev.draw(op);
}

/// Compiles on the calling thread, outside the lock.
impl Perform<Opening<CreateProgram<'_>>> for Ash {
    fn perform(&mut self, Opening(op, _): Opening<CreateProgram<'_>>) -> Result<Program, GpuError> {
        let built = Dev::build_program(&self.shared.device, &op)?;
        Ok(self.dev().add_program(built))
    }
}

/// A program's description, owned, for compiling on another thread.
struct ProgramSpec {
    label: String,
    compute: Option<(Vec<u32>, String)>,
    draw: Option<DrawSpec>,
    bindings: Vec<BindingKind>,
    push_constants: u32,
}

struct DrawSpec {
    vertex: Vec<u32>,
    vertex_entry: String,
    fragment: Vec<u32>,
    fragment_entry: String,
    attrs: Vec<crate::VertexAttr>,
    stride: u32,
    target: Format,
    blend: Blend,
}

impl ProgramSpec {
    fn of(op: &CreateProgram<'_>) -> Self {
        let (compute, draw) = match op.kind {
            ProgramKind::Compute { spirv, entry } => (Some((spirv.to_vec(), entry.into())), None),
            ProgramKind::Draw {
                vertex,
                vertex_entry,
                fragment,
                fragment_entry,
                attrs,
                stride,
                target,
                blend,
            } => (
                None,
                Some(DrawSpec {
                    vertex: vertex.to_vec(),
                    vertex_entry: vertex_entry.into(),
                    fragment: fragment.to_vec(),
                    fragment_entry: fragment_entry.into(),
                    attrs: attrs.to_vec(),
                    stride,
                    target,
                    blend,
                }),
            ),
        };
        Self {
            label: op.label.into(),
            compute,
            draw,
            bindings: op.bindings.to_vec(),
            push_constants: op.push_constants,
        }
    }

    fn op(&self) -> CreateProgram<'_> {
        let kind = match (&self.compute, &self.draw) {
            (Some((spirv, entry)), _) => ProgramKind::Compute { spirv, entry },
            (_, Some(d)) => ProgramKind::Draw {
                vertex: &d.vertex,
                vertex_entry: &d.vertex_entry,
                fragment: &d.fragment,
                fragment_entry: &d.fragment_entry,
                attrs: &d.attrs,
                stride: d.stride,
                target: d.target,
                blend: d.blend,
            },
            _ => unreachable!("one of the two"),
        };
        CreateProgram {
            label: &self.label,
            kind,
            bindings: &self.bindings,
            push_constants: self.push_constants,
        }
    }
}

type Built = Result<(ResInfo, Kind), GpuError>;

/// A program compiling on its own thread, for the async forms.
#[derive(Default)]
struct Compiling {
    state: Mutex<CompilingState>,
}

#[derive(Default)]
struct CompilingState {
    built: Option<Built>,
    waker: Option<Waker>,
    /// The future waiting for it was dropped: the thread destroys what it
    /// built.
    abandoned: bool,
}

/// Awaits a [`Compiling`]; dropping it abandons the program.
struct AwaitBuilt {
    compiling: Arc<Compiling>,
    device: ash::Device,
}

impl Future for AwaitBuilt {
    type Output = Built;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Built> {
        let mut state = self
            .compiling
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match state.built.take() {
            Some(built) => Poll::Ready(built),
            None => {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl Drop for AwaitBuilt {
    fn drop(&mut self) {
        let mut state = self
            .compiling
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.abandoned = true;
        if let Some(Ok((_, kind))) = state.built.take() {
            destroy_kind(&self.device, kind);
        }
    }
}

impl Ash {
    /// Compiles `op` on a thread of its own (shaders take a while to
    /// compile), so an executor's thread is never blocked by it.
    async fn create_program_async(&self, op: &CreateProgram<'_>) -> Result<Program, GpuError> {
        validate::create_program(op)?;
        let spec = ProgramSpec::of(op);
        let compiling = Arc::new(Compiling::default());
        let device = self.shared.device.clone();
        let theirs = (Arc::clone(&compiling), device.clone());
        std::thread::Builder::new()
            .name("seam-ash compile".to_string())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let (compiling, device) = theirs;
                let built = Dev::build_program(&device, &spec.op());
                let mut state = compiling
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if state.abandoned {
                    if let Ok((_, kind)) = built {
                        destroy_kind(&device, kind);
                    }
                    return;
                }
                state.built = Some(built);
                if let Some(waker) = state.waker.take() {
                    waker.wake();
                }
            })
            .map_err(|_| GpuError::Unsupported("could not start a thread to compile on".into()))?;
        let built = AwaitBuilt { compiling, device }.await?;
        Ok(self.dev().add_program(built))
    }
}

impl<'a> seam_core::future::AsyncPerform<Opening<CreateProgram<'a>>> for Ash {
    async fn perform_async(
        &mut self,
        Opening(op, _): Opening<CreateProgram<'a>>,
    ) -> Result<Program, GpuError> {
        self.create_program_async(&op).await
    }
}

impl<'a> seam_core::future::SendPerform<Opening<CreateProgram<'a>>> for Ash {
    async fn perform_send(
        &mut self,
        Opening(op, _): Opening<CreateProgram<'a>>,
    ) -> Result<Program, GpuError> {
        self.create_program_async(&op).await
    }
}

impl Perform<Download<'_>> for Ash {
    fn perform(&mut self, op: Download<'_>) -> Result<(), GpuError> {
        let len = op.out.len() as u64;
        let pending = {
            let mut dev = self.dev();
            validate::download(&*dev, op.buffer.raw(), op.offset, len)?;
            if len == 0 {
                return Ok(());
            }
            dev.start_download(op.buffer.raw(), op.offset, len, false)?
        };
        let fence = pending.fence;
        let progress = InProgress {
            ash: self,
            pending: Some(pending),
        };
        self.block(fence)?;
        progress.finish(Some(op.out))
    }
}

impl Perform<DownloadImage<'_>> for Ash {
    fn perform(&mut self, op: DownloadImage<'_>) -> Result<(), GpuError> {
        let pending = {
            let mut dev = self.dev();
            validate::whole_image(&*dev, op.image.raw(), op.out.len(), "download")?;
            dev.start_download(op.image.raw(), 0, op.out.len() as u64, true)?
        };
        let fence = pending.fence;
        let progress = InProgress {
            ash: self,
            pending: Some(pending),
        };
        self.block(fence)?;
        progress.finish(Some(op.out))
    }
}

impl Perform<Finish> for Ash {
    fn perform(&mut self, _: Finish) -> Result<(), GpuError> {
        // Bound first: the lock is released before waiting.
        let Some(pending) = self.dev().start_finish()? else {
            return Ok(());
        };
        let fence = pending.fence;
        let progress = InProgress {
            ash: self,
            pending: Some(pending),
        };
        self.block(fence)?;
        progress.finish(None)
    }
}

/// The async forms of the waiting operations: the same sync code before
/// and after the wait, and the wait awaited.
macro_rules! waiting_async {
    ($($method:ident: $trait:ident;)*) => {$(
        impl<'a> seam_core::future::$trait<Download<'a>> for Ash {
            async fn $method(&mut self, op: Download<'a>) -> Result<(), GpuError> {
                let len = op.out.len() as u64;
                let pending = {
                    let mut dev = self.dev();
                    validate::download(&*dev, op.buffer.raw(), op.offset, len)?;
                    if len == 0 {
                        return Ok(());
                    }
                    dev.start_download(op.buffer.raw(), op.offset, len, false)?
                };
                let (seq, fence) = (pending.seq, pending.fence);
                let progress = InProgress { ash: self, pending: Some(pending) };
                self.wait(seq, fence).await?;
                progress.finish(Some(op.out))
            }
        }

        impl<'a> seam_core::future::$trait<DownloadImage<'a>> for Ash {
            async fn $method(&mut self, op: DownloadImage<'a>) -> Result<(), GpuError> {
                let pending = {
                    let mut dev = self.dev();
                    validate::whole_image(&*dev, op.image.raw(), op.out.len(), "download")?;
                    dev.start_download(op.image.raw(), 0, op.out.len() as u64, true)?
                };
                let (seq, fence) = (pending.seq, pending.fence);
                let progress = InProgress { ash: self, pending: Some(pending) };
                self.wait(seq, fence).await?;
                progress.finish(Some(op.out))
            }
        }

        impl seam_core::future::$trait<Finish> for Ash {
            async fn $method(&mut self, _: Finish) -> Result<(), GpuError> {
                let Some(pending) = self.dev().start_finish()? else {
                    return Ok(());
                };
                let (seq, fence) = (pending.seq, pending.fence);
                let progress = InProgress { ash: self, pending: Some(pending) };
                self.wait(seq, fence).await?;
                progress.finish(None)
            }
        }
    )*};
}

waiting_async! {
    perform_async: AsyncPerform;
    perform_send: SendPerform;
}

/// Work submitted with [`Submit`] on [`Ash`]: inspect [`IsDone`] on it;
/// closing its scope waits for it. It keeps its batch's fence alive while
/// it lives.
pub struct Submission {
    ash: Ash,
    /// The batch, and its fence; `None` when there was nothing to submit.
    batch: Option<(u64, vk::Fence)>,
    done: bool,
}

impl core::fmt::Debug for Submission {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Submission")
            .field("batch", &self.batch.map(|b| b.0))
            .field("done", &self.done)
            .finish()
    }
}

impl Drop for Submission {
    fn drop(&mut self) {
        if let Some((seq, _)) = self.batch {
            let mut dev = self.ash.dev();
            dev.hold(seq, -1);
            dev.retire();
        }
    }
}

impl Dev {
    /// Submits the batch, if any, and holds the last batch in flight for a
    /// submission.
    fn start_submit(&mut self) -> Result<Option<(u64, vk::Fence)>, GpuError> {
        self.submit()?;
        self.retire();
        let last = self.in_flight.back().map(|f| (f.seq, f.fence));
        if let Some((seq, _)) = last {
            self.hold(seq, 1);
        }
        Ok(last)
    }
}

impl Ash {
    fn open_submission(&self) -> Result<Submission, GpuError> {
        let batch = self.dev().start_submit()?;
        Ok(Submission {
            ash: self.clone(),
            batch,
            done: batch.is_none(),
        })
    }
}

impl Perform<Opening<Submit<Submission>>> for Ash {
    fn perform(&mut self, _: Opening<Submit<Submission>>) -> Result<Submission, GpuError> {
        self.open_submission()
    }
}

impl Perform<Closing<Submit<Submission>>> for Ash {
    fn perform(&mut self, Closing(s, _): Closing<Submit<Submission>>) -> Result<(), GpuError> {
        match (s.done, s.batch) {
            (false, Some((_, fence))) => self.block(fence),
            _ => Ok(()),
        }
    }
}

impl Inspect<IsDone> for Submission {
    fn inspect(&self, _: IsDone) -> Result<bool, GpuError> {
        match (self.done, self.batch) {
            (false, Some((_, fence))) => self.ash.signaled(fence),
            _ => Ok(true),
        }
    }
}

macro_rules! submission_async {
    ($($method:ident, $imethod:ident: $trait:ident, $itrait:ident;)*) => {$(
        impl seam_core::future::$trait<Opening<Submit<Submission>>> for Ash {
            async fn $method(&mut self, _: Opening<Submit<Submission>>) -> Result<Submission, GpuError> {
                self.open_submission()
            }
        }

        impl seam_core::future::$trait<Closing<Submit<Submission>>> for Ash {
            async fn $method(&mut self, Closing(s, _): Closing<Submit<Submission>>) -> Result<(), GpuError> {
                match (s.done, s.batch) {
                    (false, Some((seq, fence))) => self.wait(seq, fence).await,
                    _ => Ok(()),
                }
            }
        }

        impl seam_core::future::$itrait<IsDone> for Submission {
            async fn $imethod(&self, op: IsDone) -> Result<bool, GpuError> {
                self.inspect(op)
            }
        }
    )*};
}

submission_async! {
    perform_async, inspect_async: AsyncPerform, AsyncInspect;
    perform_send, inspect_send: SendPerform, SendInspect;
}

impl Inspect<DeviceInfo> for Ash {
    fn inspect(&self, _: DeviceInfo) -> GpuInfo {
        self.shared.info.clone()
    }
}

impl seam_core::future::AsyncInspect<DeviceInfo> for Ash {
    async fn inspect_async(&self, _: DeviceInfo) -> GpuInfo {
        self.shared.info.clone()
    }
}

impl seam_core::future::SendInspect<DeviceInfo> for Ash {
    async fn inspect_send(&self, _: DeviceInfo) -> GpuInfo {
        self.shared.info.clone()
    }
}

// After `now!`, which it uses.
mod import;
