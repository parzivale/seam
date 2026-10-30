//! Swapchains: a surface on a window, and the images shown on it in turn.
//!
//! Each image `NextFrame` acquires is lent as a new [`Image`] (its layout
//! undefined, as an acquired image's contents are), and retired by
//! `Present`. The batch that draws into it waits on the acquire's
//! semaphore, and signals the image's own semaphore for the presentation
//! to wait on.

use std::format;
use std::vec::Vec;

use ash::vk;
#[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
use raw_window_handle::RawDisplayHandle;
#[cfg(any(windows, all(unix, not(target_vendor = "apple"), not(target_os = "android"))))]
use raw_window_handle::RawWindowHandle;

use super::{Dev, Foreign, Kind, Use, vk_error, vk_format};
use crate::validate;
use crate::{
    CreateSwapchain, GpuError, Image, ImageUsage, NextFrame, Present, PresentMode, ResInfo,
    ResizeSwapchain, Swapchain, Window,
};

/// The instance extensions surfaces need, on this platform.
pub(super) fn instance_extensions() -> Vec<&'static core::ffi::CStr> {
    #[allow(unused_mut)]
    let mut names = std::vec![vk::KHR_SURFACE_NAME, vk::EXT_HEADLESS_SURFACE_NAME];
    #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
    names.extend([
        vk::KHR_XLIB_SURFACE_NAME,
        vk::KHR_XCB_SURFACE_NAME,
        vk::KHR_WAYLAND_SURFACE_NAME,
    ]);
    #[cfg(windows)]
    names.push(vk::KHR_WIN32_SURFACE_NAME);
    names
}

/// A swapchain's Vulkan objects.
pub(super) struct Chain {
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    /// Signalled when each image's frame is drawn, for presenting it.
    done: Vec<vk::Semaphore>,
    /// The image lent, and its index.
    lent: Option<(u64, u32)>,
    op: CreateSwapchain,
}

fn unsupported(why: &str) -> GpuError {
    GpuError::Unsupported(why.into())
}

impl Dev {
    /// The surface for `window`.
    fn surface(&self, window: Window) -> Result<vk::SurfaceKHR, GpuError> {
        let no = |what: &str| unsupported(&format!("this Vulkan has no {what} surfaces"));
        // SAFETY: the handles name a live window (`CreateSwapchain::new`'s
        // contract), with the matching surface extension on.
        unsafe {
            match window {
                Window::Headless => {
                    let info = vk::HeadlessSurfaceCreateInfoEXT::default();
                    ash::ext::headless_surface::Instance::new(&self.entry, &self.instance)
                        .create_headless_surface(&info, None)
                        .map_err(|_| no("headless"))
                }
                #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
                Window::Raw {
                    display: RawDisplayHandle::Xlib(d),
                    window: RawWindowHandle::Xlib(w),
                } => {
                    let dpy = d.display.ok_or_else(|| no("Xlib"))?.as_ptr();
                    let info = vk::XlibSurfaceCreateInfoKHR::default()
                        .dpy(dpy.cast())
                        .window(w.window);
                    ash::khr::xlib_surface::Instance::new(&self.entry, &self.instance)
                        .create_xlib_surface(&info, None)
                        .map_err(|_| no("Xlib"))
                }
                #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
                Window::Raw {
                    display: RawDisplayHandle::Xcb(d),
                    window: RawWindowHandle::Xcb(w),
                } => {
                    let connection = d.connection.ok_or_else(|| no("XCB"))?.as_ptr();
                    let info = vk::XcbSurfaceCreateInfoKHR::default()
                        .connection(connection)
                        .window(w.window.get());
                    ash::khr::xcb_surface::Instance::new(&self.entry, &self.instance)
                        .create_xcb_surface(&info, None)
                        .map_err(|_| no("XCB"))
                }
                #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
                Window::Raw {
                    display: RawDisplayHandle::Wayland(d),
                    window: RawWindowHandle::Wayland(w),
                } => {
                    let info = vk::WaylandSurfaceCreateInfoKHR::default()
                        .display(d.display.as_ptr())
                        .surface(w.surface.as_ptr());
                    ash::khr::wayland_surface::Instance::new(&self.entry, &self.instance)
                        .create_wayland_surface(&info, None)
                        .map_err(|_| no("Wayland"))
                }
                #[cfg(windows)]
                Window::Raw {
                    window: RawWindowHandle::Win32(w),
                    ..
                } => {
                    let info = vk::Win32SurfaceCreateInfoKHR::default()
                        .hinstance(w.hinstance.map_or(0, |h| h.get()))
                        .hwnd(w.hwnd.get());
                    ash::khr::win32_surface::Instance::new(&self.entry, &self.instance)
                        .create_win32_surface(&info, None)
                        .map_err(|_| no("Win32"))
                }
                Window::Raw { window, .. } => Err(unsupported(&format!(
                    "seam-ash does not present to {window:?} windows"
                ))),
            }
        }
    }

    /// Makes `op`'s swapchain on `surface`, replacing `old`.
    fn swapchain(
        &self,
        surface: vk::SurfaceKHR,
        op: &CreateSwapchain,
        old: vk::SwapchainKHR,
    ) -> Result<(vk::SwapchainKHR, Vec<vk::Image>), GpuError> {
        let (Some(surfaces), Some(chains)) = (&self.ext.surface, &self.ext.swapchain) else {
            return Err(unsupported("this device has no swapchains"));
        };
        // SAFETY: plain queries on a live surface.
        let (supported, caps, formats, modes) = unsafe {
            (
                surfaces
                    .get_physical_device_surface_support(self.physical, self.family, surface)
                    .map_err(vk_error)?,
                surfaces
                    .get_physical_device_surface_capabilities(self.physical, surface)
                    .map_err(vk_error)?,
                surfaces
                    .get_physical_device_surface_formats(self.physical, surface)
                    .map_err(vk_error)?,
                surfaces
                    .get_physical_device_surface_present_modes(self.physical, surface)
                    .map_err(vk_error)?,
            )
        };
        if !supported {
            return Err(unsupported(
                "this device's queue cannot present to the window",
            ));
        }
        let format = vk_format(op.format);
        let Some(chosen) = formats
            .iter()
            .find(|f| f.format == format && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
        else {
            return Err(unsupported(&format!(
                "the window does not take {:?} images",
                op.format
            )));
        };
        let mut usage = vk::ImageUsageFlags::COLOR_ATTACHMENT
            | vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::TRANSFER_DST;
        if op.usage.contains(ImageUsage::SAMPLED) {
            usage |= vk::ImageUsageFlags::SAMPLED;
        }
        if op.usage.contains(ImageUsage::STORAGE) {
            usage |= vk::ImageUsageFlags::STORAGE;
        }
        if !caps.supported_usage_flags.contains(usage) {
            return Err(unsupported(&format!(
                "the window's images cannot be {:?}",
                op.usage
            )));
        }
        let mode = match op.mode {
            PresentMode::Fifo => vk::PresentModeKHR::FIFO,
            PresentMode::Mailbox => vk::PresentModeKHR::MAILBOX,
            PresentMode::Immediate => vk::PresentModeKHR::IMMEDIATE,
        };
        let mode = if modes.contains(&mode) {
            mode
        } else {
            vk::PresentModeKHR::FIFO
        };
        let mut count = (caps.min_image_count + 1).max(2);
        if caps.max_image_count > 0 {
            count = count.min(caps.max_image_count);
        }
        let alpha = [
            vk::CompositeAlphaFlagsKHR::OPAQUE,
            vk::CompositeAlphaFlagsKHR::INHERIT,
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
            vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
        ]
        .into_iter()
        .find(|a| caps.supported_composite_alpha.contains(*a))
        .unwrap_or(vk::CompositeAlphaFlagsKHR::OPAQUE);
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(count)
            .image_format(chosen.format)
            .image_color_space(chosen.color_space)
            .image_extent(vk::Extent2D {
                width: op.width,
                height: op.height,
            })
            .image_array_layers(1)
            .image_usage(usage)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(alpha)
            .present_mode(mode)
            .clipped(true)
            .old_swapchain(old);
        // SAFETY: a valid create info on a live surface; the images belong
        // to the swapchain.
        unsafe {
            let swapchain = chains.create_swapchain(&info, None).map_err(vk_error)?;
            match chains.get_swapchain_images(swapchain) {
                Ok(images) => Ok((swapchain, images)),
                Err(e) => {
                    chains.destroy_swapchain(swapchain, None);
                    Err(vk_error(e))
                }
            }
        }
    }

    fn semaphores(&self, n: usize) -> Result<Vec<vk::Semaphore>, GpuError> {
        (0..n)
            .map(|_| {
                // SAFETY: a valid create info.
                unsafe {
                    self.device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                }
                .map_err(vk_error)
            })
            .collect()
    }

    pub(super) fn create_swapchain(&mut self, op: CreateSwapchain) -> Result<Swapchain, GpuError> {
        validate::create_swapchain(&op)?;
        if self.ext.surface.is_none() {
            return Err(unsupported("this Vulkan has no surfaces"));
        }
        let surface = self.surface(op.window)?;
        let destroy_surface = |dev: &Self| {
            // SAFETY: nothing uses it.
            unsafe {
                dev.ext
                    .surface
                    .as_ref()
                    .expect("checked")
                    .destroy_surface(surface, None)
            };
        };
        let (swapchain, images) = match self.swapchain(surface, &op, vk::SwapchainKHR::null()) {
            Ok(made) => made,
            Err(e) => {
                destroy_surface(self);
                return Err(e);
            }
        };
        let done = self.semaphores(images.len())?;
        let info = ResInfo::Swapchain {
            width: op.width,
            height: op.height,
            format: op.format,
            usage: op.usage,
            lent: None,
        };
        let chain = Chain {
            surface,
            swapchain,
            images,
            done,
            lent: None,
            op,
        };
        Ok(Swapchain(self.add(info, Kind::Swapchain(Box::new(chain)))))
    }

    fn chain(&mut self, id: u64) -> &mut Chain {
        match &mut self.res.get_mut(&id).expect("validated").kind {
            Kind::Swapchain(c) => c,
            _ => unreachable!("validated as a swapchain"),
        }
    }

    fn lend(&mut self, id: u64, image: Option<u64>) {
        if let Some(ResInfo::Swapchain { lent, .. }) = self.res.get_mut(&id).map(|r| &mut r.info) {
            *lent = image;
        }
    }

    pub(super) fn next_frame(&mut self, op: NextFrame) -> Result<Image, GpuError> {
        let image = validate::next_frame(&*self, &op)?;
        self.retire();
        let acquired = self.semaphores(1)?[0];
        let chains = self.ext.swapchain.clone().expect("a swapchain exists");
        let swapchain = self.chain(op.0.raw()).swapchain;
        // SAFETY: a live swapchain, a new semaphore; waits for an image.
        let got =
            unsafe { chains.acquire_next_image(swapchain, u64::MAX, acquired, vk::Fence::null()) };
        let index = match got {
            Ok((index, _suboptimal)) => index,
            Err(e) => {
                // SAFETY: nothing waits on it.
                unsafe { self.device.destroy_semaphore(acquired, None) };
                return Err(match e {
                    vk::Result::ERROR_OUT_OF_DATE_KHR => GpuError::Outdated,
                    e => vk_error(e),
                });
            }
        };
        let raw = self.chain(op.0.raw()).images[index as usize];
        let view = self.view(raw, &image, None, false)?;
        let kind = Kind::image(
            raw,
            vk::DeviceMemory::null(),
            view,
            vk::ImageLayout::UNDEFINED,
            Foreign::Swapchain,
            image.format,
        );
        let id = self.add(ResInfo::Image(image), kind);
        // The semaphore's wait is at every stage: the first barrier (its
        // layout change) chains after it.
        self.res.get_mut(&id).expect("just added").track.reads =
            vk::PipelineStageFlags2::ALL_COMMANDS;
        let batch = self.batch()?;
        batch.waits.push(acquired);
        batch.semaphores.push(acquired);
        self.chain(op.0.raw()).lent = Some((id, index));
        self.lend(op.0.raw(), Some(id));
        Ok(Image(id))
    }

    pub(super) fn present(&mut self, op: Present) -> Result<(), GpuError> {
        let image = validate::present(&*self, &op)?;
        self.retire();
        // Into the layout presenting reads, after everything drawn.
        self.uses(&[Use {
            id: image,
            stage: vk::PipelineStageFlags2::ALL_COMMANDS,
            access: vk::AccessFlags2::NONE,
            write: false,
            layout: Some(vk::ImageLayout::PRESENT_SRC_KHR),
        }])?;
        let chain = self.chain(op.0.raw());
        let (_, index) = chain.lent.take().expect("validated: an image is lent");
        let (swapchain, done) = (chain.swapchain, chain.done[index as usize]);
        self.batch()?.signals.push(done);
        self.submit()?;
        self.lend(op.0.raw(), None);
        self.destroy(image);
        let chains = self.ext.swapchain.clone().expect("a swapchain exists");
        let (waits, swapchains, indices) = ([done], [swapchain], [index]);
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&waits)
            .swapchains(&swapchains)
            .image_indices(&indices);
        // SAFETY: the image was acquired and its frame submitted, signalling
        // `done`; the queue is used only under the lock.
        match unsafe { chains.queue_present(self.queue, &info) } {
            Ok(_) => Ok(()),
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => Err(GpuError::Outdated),
            Err(e) => Err(vk_error(e)),
        }
    }

    /// Waits until nothing uses the swapchain's images.
    fn idle(&mut self) -> Result<(), GpuError> {
        self.submit()?;
        // SAFETY: the queue is used only under the lock.
        unsafe { self.device.queue_wait_idle(self.queue) }.map_err(vk_error)?;
        self.retire();
        Ok(())
    }

    pub(super) fn resize_swapchain(&mut self, op: ResizeSwapchain) -> Result<(), GpuError> {
        validate::resize(&*self, &op)?;
        self.idle()?;
        let id = op.swapchain.raw();
        let (surface, old, mut made) = {
            let chain = self.chain(id);
            let mut made = chain.op;
            (made.width, made.height) = (op.width, op.height);
            (chain.surface, chain.swapchain, made)
        };
        made.window = Window::Headless; // Unused: the surface is kept.
        let (swapchain, images) = self.swapchain(surface, &made, old)?;
        let done = self.semaphores(images.len())?;
        let chains = self.ext.swapchain.clone().expect("a swapchain exists");
        let chain = self.chain(id);
        let old_done = core::mem::replace(&mut chain.done, done);
        chain.swapchain = swapchain;
        chain.images = images;
        (chain.op.width, chain.op.height) = (op.width, op.height);
        // SAFETY: the queue is idle: nothing uses the old swapchain.
        unsafe {
            chains.destroy_swapchain(old, None);
            for s in old_done {
                self.device.destroy_semaphore(s, None);
            }
        }
        if let Some(ResInfo::Swapchain { width, height, .. }) =
            self.res.get_mut(&id).map(|r| &mut r.info)
        {
            (*width, *height) = (op.width, op.height);
        }
        Ok(())
    }

    pub(super) fn destroy_swapchain(&mut self, swapchain: Swapchain) -> Result<(), GpuError> {
        let id = swapchain.raw();
        if !self.res.contains_key(&id) {
            return Ok(());
        }
        if let Some((image, _)) = self.chain(id).lent {
            self.destroy(image);
        }
        self.idle()?;
        let Some(res) = self.res.remove(&id) else {
            return Ok(());
        };
        let Kind::Swapchain(chain) = res.kind else {
            unreachable!("a swapchain")
        };
        // SAFETY: the queue is idle: nothing uses the swapchain, its
        // images or its semaphores.
        unsafe {
            if let Some(chains) = &self.ext.swapchain {
                chains.destroy_swapchain(chain.swapchain, None);
            }
            for s in chain.done {
                self.device.destroy_semaphore(s, None);
            }
            if let Some(surfaces) = &self.ext.surface {
                surfaces.destroy_surface(chain.surface, None);
            }
        }
        Ok(())
    }
}
