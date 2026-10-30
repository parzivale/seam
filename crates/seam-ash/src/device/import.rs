//! [`Import`]: images from outside this device, by platform.
//!
//! An imported image is an ordinary resource once open, with one
//! difference: its memory belongs to someone else (a Wayland client, a
//! Direct3D device), so Vulkan takes it over by a queue family ownership
//! transfer at its first use (`Foreign::Acquire`), and hands it back when
//! its scope closes. Closing then waits for the GPU to finish with it, so
//! the owner may change it as soon as the scope has closed.

use super::*;

impl Dev {
    /// Ends an import's scope: hands its memory back to its owner, destroys
    /// it once unused, and holds the batch to wait for before the owner may
    /// touch it, if one is still in flight.
    pub(super) fn start_close_import(&mut self, id: u64) -> Result<Option<Pending>, GpuError> {
        let family = self.family;
        let held = matches!(
            self.res.get(&id).map(|r| &r.kind),
            Some(Kind::Image {
                foreign: Foreign::Held(_),
                ..
            })
        );
        if held {
            let (seq, cmd) = {
                let b = self.batch()?;
                (b.seq, b.cmd)
            };
            let res = self.res.get_mut(&id).expect("checked");
            let t = &res.track;
            let prior_stage = t.write.map_or(vk::PipelineStageFlags2::NONE, |w| w.0) | t.reads;
            let prior_access = t.write.map_or(vk::AccessFlags2::NONE, |w| w.1);
            let Kind::Image {
                image,
                layout,
                foreign,
                ..
            } = &mut res.kind
            else {
                unreachable!("checked")
            };
            let Foreign::Held(owner) = *foreign else {
                unreachable!("checked")
            };
            let release = [vk::ImageMemoryBarrier2::default()
                .src_stage_mask(prior_stage)
                .src_access_mask(prior_access)
                .old_layout(*layout)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(family)
                .dst_queue_family_index(owner)
                .image(*image)
                .subresource_range(super::whole(vk::ImageAspectFlags::COLOR))];
            // SAFETY: `cmd` is recording and the image is alive.
            unsafe {
                self.device.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::default().image_memory_barriers(&release),
                );
            }
            *layout = vk::ImageLayout::GENERAL;
            *foreign = Foreign::No;
            res.last_use = seq;
        }
        let last = self.res.get(&id).map_or(0, |r| r.last_use);
        self.destroy(id);
        if last <= self.completed {
            return Ok(None);
        }
        if self.batch.as_ref().is_some_and(|b| b.seq <= last) {
            self.submit()?;
        }
        self.retire();
        Ok(self.in_flight.iter_mut().find(|f| f.seq == last).map(|f| {
            f.holds += 1;
            Pending {
                seq: f.seq,
                fence: f.fence,
                staging: None,
            }
        }))
    }
}

impl Ash {
    /// Closes an import's scope: waits until its owner may have it back.
    fn close_import(&self, image: Image) -> Result<(), GpuError> {
        let Some(pending) = self.dev().start_close_import(image.raw())? else {
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

    async fn close_import_async(&self, image: Image) -> Result<(), GpuError> {
        let Some(pending) = self.dev().start_close_import(image.raw())? else {
            return Ok(());
        };
        let (seq, fence) = (pending.seq, pending.fence);
        let progress = InProgress {
            ash: self,
            pending: Some(pending),
        };
        self.wait(seq, fence).await?;
        progress.finish(None)
    }
}

impl<B> Perform<Closing<Import<B>>> for Ash {
    fn perform(&mut self, Closing(image, _): Closing<Import<B>>) -> Result<(), GpuError> {
        self.close_import(image)
    }
}

impl<B> seam_core::future::AsyncPerform<Closing<Import<B>>> for Ash {
    async fn perform_async(
        &mut self,
        Closing(image, _): Closing<Import<B>>,
    ) -> Result<(), GpuError> {
        self.close_import_async(image).await
    }
}

impl<B> seam_core::future::SendPerform<Closing<Import<B>>> for Ash {
    async fn perform_send(
        &mut self,
        Closing(image, _): Closing<Import<B>>,
    ) -> Result<(), GpuError> {
        self.close_import_async(image).await
    }
}

/// The lowest memory type in `bits`.
#[cfg(any(drm, windows))]
fn first_type(bits: u32) -> Option<u32> {
    (bits != 0).then(|| bits.trailing_zeros())
}

// Linux and the BSDs: dmabufs.

#[cfg(drm)]
mod dmabuf {
    use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};

    use rustix::event::{PollFd, PollFlags, Timespec};

    use super::*;
    use crate::drm::{Dmabuf, DmabufFormat, DmabufFormats, Fourcc, Modifier};
    use crate::invalid;

    const DMA_BUF: vk::ExternalMemoryHandleTypeFlags =
        vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;

    impl Dev {
        /// The queue family imports come from.
        fn foreign_family(&self) -> u32 {
            if self.ext.foreign {
                vk::QUEUE_FAMILY_FOREIGN_EXT
            } else {
                vk::QUEUE_FAMILY_EXTERNAL
            }
        }

        /// The modifiers `format` has on this device, with their features.
        fn modifiers(&self, format: vk::Format) -> Vec<vk::DrmFormatModifierPropertiesEXT> {
            if !self.ext.modifiers {
                return Vec::new();
            }
            // SAFETY: plain queries, the second into a buffer of the size
            // the first gave.
            unsafe {
                let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
                let mut props = vk::FormatProperties2::default().push_next(&mut list);
                self.instance.get_physical_device_format_properties2(
                    self.physical,
                    format,
                    &mut props,
                );
                let mut out = std::vec![
                    vk::DrmFormatModifierPropertiesEXT::default();
                    list.drm_format_modifier_count as usize
                ];
                let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
                    .drm_format_modifier_properties(&mut out);
                let mut props = vk::FormatProperties2::default().push_next(&mut list);
                self.instance.get_physical_device_format_properties2(
                    self.physical,
                    format,
                    &mut props,
                );
                let n = list.drm_format_modifier_count as usize;
                out.truncate(n);
                out
            }
        }

        pub(super) fn dmabuf_formats(&self) -> Vec<DmabufFormat> {
            let mut out = Vec::new();
            if self.ext.dmabuf.is_none() {
                return out;
            }
            for fourcc in Fourcc::SUPPORTED {
                let (format, _) = fourcc.format().expect("supported");
                let format = vk_format(format);
                for m in self.modifiers(format) {
                    if m.drm_format_modifier_plane_count == 1
                        && m.drm_format_modifier_tiling_features
                            .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE)
                    {
                        out.push(DmabufFormat {
                            fourcc,
                            modifier: Modifier(m.drm_format_modifier),
                        });
                    }
                }
                // Linear ones are copied into an image, where the device
                // cannot import them as one.
                // SAFETY: a plain query.
                let props = unsafe {
                    self.instance
                        .get_physical_device_format_properties(self.physical, format)
                };
                let linear = DmabufFormat {
                    fourcc,
                    modifier: Modifier::LINEAR,
                };
                if props.optimal_tiling_features.contains(
                    vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::TRANSFER_DST,
                ) && !out.contains(&linear)
                {
                    out.push(linear);
                }
            }
            out
        }

        /// Imports memory from `fd`, for `reqs`, dedicated to an image or a
        /// buffer.
        ///
        /// # Safety
        ///
        /// `image` or `buffer` is alive and unbound, with `reqs` its
        /// requirements.
        unsafe fn import_fd(
            &self,
            fns: &ash::khr::external_memory_fd::Device,
            fd: &OwnedFd,
            reqs: vk::MemoryRequirements,
            image: vk::Image,
            buffer: vk::Buffer,
        ) -> Result<vk::DeviceMemory, GpuError> {
            let dup = fd
                .try_clone()
                .map_err(|e| invalid(format!("the dmabuf's descriptor: {e}")))?;
            let mut fd_props = vk::MemoryFdPropertiesKHR::default();
            // SAFETY: a valid descriptor, borrowed for the call.
            unsafe { fns.get_memory_fd_properties(DMA_BUF, dup.as_raw_fd(), &mut fd_props) }
                .map_err(vk_error)?;
            let index =
                first_type(reqs.memory_type_bits & fd_props.memory_type_bits).ok_or_else(|| {
                    GpuError::Unsupported("no memory type imports this dmabuf".into())
                })?;
            let mut import = vk::ImportMemoryFdInfoKHR::default()
                .handle_type(DMA_BUF)
                .fd(dup.as_raw_fd());
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default()
                .image(image)
                .buffer(buffer);
            let info = vk::MemoryAllocateInfo::default()
                .allocation_size(reqs.size)
                .memory_type_index(index)
                .push_next(&mut import)
                .push_next(&mut dedicated);
            // SAFETY: per the caller; on success Vulkan owns the
            // descriptor, on failure it is still ours and closed by `dup`.
            let memory = unsafe { self.device.allocate_memory(&info, None) }.map_err(vk_error)?;
            let _ = dup.into_raw_fd();
            Ok(memory)
        }

        pub(super) fn import_dmabuf(
            &mut self,
            buf: &Dmabuf,
            usage: ImageUsage,
        ) -> Result<Image, GpuError> {
            validate::dmabuf(buf, usage)?;
            let fns = self.ext.dmabuf.clone().ok_or_else(|| {
                GpuError::Unsupported("this device does not import dmabufs".into())
            })?;
            self.retire();
            let (format, opaque) = buf.format.format().expect("validated");
            let vkf = vk_format(format);
            let (_, features) = Self::image_usage(usage, format);
            let wanted = features
                & !(vk::FormatFeatureFlags::TRANSFER_SRC | vk::FormatFeatureFlags::TRANSFER_DST);
            let modifier = self.modifiers(vkf).into_iter().find(|m| {
                m.drm_format_modifier == buf.modifier.0
                    && m.drm_format_modifier_plane_count == 1
                    && m.drm_format_modifier_tiling_features.contains(wanted)
            });
            match modifier {
                Some(m) => self.import_dmabuf_image(&fns, buf, format, usage, opaque, m),
                None if buf.modifier == Modifier::LINEAR => {
                    self.import_dmabuf_copy(&fns, buf, format, usage, opaque)
                }
                None => Err(GpuError::Unsupported(format!(
                    "{:?} dmabufs with modifier {:#x} on this device; see DmabufFormats",
                    buf.format, buf.modifier.0
                ))),
            }
        }

        /// Imports the dmabuf as the image itself: no copy.
        fn import_dmabuf_image(
            &mut self,
            fns: &ash::khr::external_memory_fd::Device,
            buf: &Dmabuf,
            format: Format,
            usage: ImageUsage,
            opaque: bool,
            modifier: vk::DrmFormatModifierPropertiesEXT,
        ) -> Result<Image, GpuError> {
            let plane = &buf.planes[0];
            let vkf = vk_format(format);
            let (all, _) = Self::image_usage(usage, format);
            // Transfers only where the layout allows them.
            let mut flags =
                all & !(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST);
            let tiling = modifier.drm_format_modifier_tiling_features;
            if tiling.contains(vk::FormatFeatureFlags::TRANSFER_SRC) {
                flags |= vk::ImageUsageFlags::TRANSFER_SRC;
            }
            if tiling.contains(vk::FormatFeatureFlags::TRANSFER_DST) {
                flags |= vk::ImageUsageFlags::TRANSFER_DST;
            }
            let layouts = [vk::SubresourceLayout {
                offset: u64::from(plane.offset),
                size: 0,
                row_pitch: u64::from(plane.stride),
                array_pitch: 0,
                depth_pitch: 0,
            }];
            let mut explicit = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
                .drm_format_modifier(buf.modifier.0)
                .plane_layouts(&layouts);
            let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(DMA_BUF);
            let info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vkf)
                .extent(vk::Extent3D {
                    width: buf.width,
                    height: buf.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                .usage(flags)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut external)
                .push_next(&mut explicit);
            // SAFETY: valid create infos; everything made is destroyed on a
            // later failure.
            let (image, memory, view) = unsafe {
                let image = self.device.create_image(&info, None).map_err(vk_error)?;
                let reqs = self.device.get_image_memory_requirements(image);
                let memory = match self.import_fd(fns, &plane.fd, reqs, image, vk::Buffer::null()) {
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
                match self.view(
                    image,
                    &CreateImage::new(buf.width, buf.height, format, usage),
                    None,
                    opaque,
                ) {
                    Ok(v) => (image, memory, v),
                    Err(e) => {
                        self.device.destroy_image(image, None);
                        self.device.free_memory(memory, None);
                        return Err(e);
                    }
                }
            };
            let id = self.add(
                ResInfo::Image(CreateImage::new(buf.width, buf.height, format, usage)),
                Kind::image(
                    image,
                    memory,
                    view,
                    // What the owner left it in: its contents are kept.
                    vk::ImageLayout::GENERAL,
                    Foreign::Acquire(self.foreign_family()),
                    format,
                ),
            );
            Ok(Image(id))
        }

        /// Imports a linear dmabuf as a buffer and copies it into an image
        /// on the GPU: for devices that cannot import it as an image.
        fn import_dmabuf_copy(
            &mut self,
            fns: &ash::khr::external_memory_fd::Device,
            buf: &Dmabuf,
            format: Format,
            usage: ImageUsage,
            opaque: bool,
        ) -> Result<Image, GpuError> {
            let plane = &buf.planes[0];
            let size = u64::from(plane.offset) + u64::from(plane.stride) * u64::from(buf.height);
            if let Ok(len) = rustix::fs::seek(&plane.fd, rustix::fs::SeekFrom::End(0))
                && len < size
            {
                return Err(invalid(format!(
                    "a {}x{} dmabuf at offset {} with stride {} needs {size} bytes; it has {len}",
                    buf.width, buf.height, plane.offset, plane.stride
                )));
            }
            let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(DMA_BUF);
            let info = vk::BufferCreateInfo::default()
                .size(size)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .push_next(&mut external);
            // SAFETY: a valid create info; everything made is destroyed on
            // a later failure.
            let (buffer, memory) = unsafe {
                let buffer = self.device.create_buffer(&info, None).map_err(vk_error)?;
                let reqs = self.device.get_buffer_memory_requirements(buffer);
                let memory = match self.import_fd(fns, &plane.fd, reqs, vk::Image::null(), buffer) {
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
                (buffer, memory)
            };
            let source = Kind::Buffer { buffer, memory };
            let image = match self.create_image_as(
                CreateImage::new(buf.width, buf.height, format, usage),
                opaque,
            ) {
                Ok(i) => i,
                Err(e) => {
                    self.destroy_kind(source);
                    return Err(e);
                }
            };
            let cmd = match self.uses(&[Use {
                id: image.raw(),
                stage: vk::PipelineStageFlags2::COPY,
                access: vk::AccessFlags2::TRANSFER_WRITE,
                write: true,
                layout: Some(vk::ImageLayout::TRANSFER_DST_OPTIMAL),
            }]) {
                Ok(c) => c,
                Err(e) => {
                    self.destroy_kind(source);
                    self.destroy(image.raw());
                    return Err(e);
                }
            };
            let seq = self.batch.as_ref().expect("just used").seq;
            let (family, owner) = (self.family, self.foreign_family());
            let barrier = |acquire: bool| {
                let b = vk::BufferMemoryBarrier2::default()
                    .buffer(buffer)
                    .offset(0)
                    .size(vk::WHOLE_SIZE);
                if acquire {
                    b.dst_stage_mask(vk::PipelineStageFlags2::COPY)
                        .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                        .src_queue_family_index(owner)
                        .dst_queue_family_index(family)
                } else {
                    b.src_stage_mask(vk::PipelineStageFlags2::COPY)
                        .src_access_mask(vk::AccessFlags2::TRANSFER_READ)
                        .src_queue_family_index(family)
                        .dst_queue_family_index(owner)
                }
            };
            let (img, mut region) = self.image_region(image.raw(), &ImageRegion::ALL);
            region.buffer_offset = u64::from(plane.offset);
            region.buffer_row_length = plane.stride / format.bytes_per_pixel();
            // SAFETY: `cmd` is recording; the buffer and image are alive,
            // and the region is in range of both (checked above).
            unsafe {
                let acquire = [barrier(true)];
                self.device.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::default().buffer_memory_barriers(&acquire),
                );
                self.device.cmd_copy_buffer_to_image(
                    cmd,
                    buffer,
                    img,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
                let release = [barrier(false)];
                self.device.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::default().buffer_memory_barriers(&release),
                );
            }
            // The dmabuf's buffer goes once the copy is done.
            self.dead.push((seq, source));
            Ok(image)
        }
    }

    /// Waits until a dmabuf's implicit fences let it be read (or written,
    /// with `write`), for up to `timeout` (`None`: as long as it takes).
    /// Whether it may be.
    fn ready(
        planes: &[&OwnedFd],
        write: bool,
        timeout: Option<Duration>,
    ) -> Result<bool, GpuError> {
        let flags = if write { PollFlags::OUT } else { PollFlags::IN };
        for fd in planes {
            let mut fds = [PollFd::new(*fd, flags)];
            let timeout = timeout.map(|t| Timespec {
                tv_sec: t.as_secs().try_into().unwrap_or(i64::MAX),
                tv_nsec: t.subsec_nanos().into(),
            });
            loop {
                match rustix::event::poll(&mut fds, timeout.as_ref()) {
                    Ok(_) => break,
                    Err(rustix::io::Errno::INTR) => {}
                    Err(e) => return Err(invalid(format!("waiting on a dmabuf: {e}"))),
                }
            }
            let got = fds[0].revents();
            if got.intersects(PollFlags::ERR | PollFlags::NVAL) {
                return Err(invalid("a dmabuf's descriptor is not pollable"));
            }
            if !got.intersects(flags) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Whether `usage` writes the image.
    fn writes(usage: ImageUsage) -> bool {
        usage.contains(ImageUsage::TARGET) || usage.contains(ImageUsage::STORAGE)
    }

    impl Ash {
        /// Waits for a dmabuf's writers on this thread: spins, then sleeps
        /// in `poll`.
        fn dmabuf_ready(&self, buf: &Dmabuf, write: bool) -> Result<(), GpuError> {
            let planes: Vec<&OwnedFd> = buf.planes.iter().map(|p| &p.fd).collect();
            if self.dmabuf_spin(&planes, write)? {
                return Ok(());
            }
            ready(&planes, write, None).map(drop)
        }

        /// Checks, then keeps checking for the spin budget.
        fn dmabuf_spin(&self, planes: &[&OwnedFd], write: bool) -> Result<bool, GpuError> {
            let until = std::time::Instant::now() + self.shared.spin;
            loop {
                if ready(planes, write, Some(Duration::ZERO))? {
                    return Ok(true);
                }
                if std::time::Instant::now() >= until {
                    return Ok(false);
                }
                core::hint::spin_loop();
            }
        }

        /// Waits for a dmabuf's writers without blocking: spins, then
        /// waits on a thread of its own (a client's buffer can take a
        /// frame or more, so not on the fence waiter).
        async fn dmabuf_ready_async(&self, buf: &Dmabuf, write: bool) -> Result<(), GpuError> {
            let planes: Vec<&OwnedFd> = buf.planes.iter().map(|p| &p.fd).collect();
            if self.dmabuf_spin(&planes, write)? {
                return Ok(());
            }
            let fds = planes
                .iter()
                .map(|fd| fd.try_clone())
                .collect::<Result<Vec<OwnedFd>, _>>()
                .map_err(|e| invalid(format!("the dmabuf's descriptor: {e}")))?;
            let signal = Arc::new(Signal::default());
            let done = Arc::clone(&signal);
            std::thread::Builder::new()
                .name("seam-ash dmabuf".to_string())
                .stack_size(self.shared.waiter_stack)
                .spawn(move || {
                    let planes: Vec<&OwnedFd> = fds.iter().collect();
                    done.complete(ready(&planes, write, None).map(drop));
                })
                .map_err(|e| GpuError::Unsupported(format!("a thread to wait on: {e}")))?;
            FenceWait(signal).await
        }
    }

    impl<'a> Perform<Opening<Import<&'a Dmabuf>>> for Ash {
        fn perform(
            &mut self,
            Opening(op, _): Opening<Import<&'a Dmabuf>>,
        ) -> Result<Image, GpuError> {
            validate::dmabuf(op.source, op.usage)?;
            self.dmabuf_ready(op.source, writes(op.usage))?;
            self.dev().import_dmabuf(op.source, op.usage)
        }
    }

    macro_rules! import_async {
        ($($method:ident: $trait:ident;)*) => {$(
            impl<'a> seam_core::future::$trait<Opening<Import<&'a Dmabuf>>> for Ash {
                async fn $method(&mut self, Opening(op, _): Opening<Import<&'a Dmabuf>>) -> Result<Image, GpuError> {
                    validate::dmabuf(op.source, op.usage)?;
                    self.dmabuf_ready_async(op.source, writes(op.usage)).await?;
                    self.dev().import_dmabuf(op.source, op.usage)
                }
            }
        )*};
    }

    import_async! {
        perform_async: AsyncPerform;
        perform_send: SendPerform;
    }

    impl Inspect<DmabufFormats> for Ash {
        fn inspect(&self, _: DmabufFormats) -> Result<Vec<DmabufFormat>, GpuError> {
            Ok(self.dev().dmabuf_formats())
        }
    }

    impl seam_core::future::AsyncInspect<DmabufFormats> for Ash {
        async fn inspect_async(&self, op: DmabufFormats) -> Result<Vec<DmabufFormat>, GpuError> {
            self.inspect(op)
        }
    }

    impl seam_core::future::SendInspect<DmabufFormats> for Ash {
        async fn inspect_send(&self, op: DmabufFormats) -> Result<Vec<DmabufFormat>, GpuError> {
            self.inspect(op)
        }
    }
}

// Windows: shared NT handles.

#[cfg(windows)]
mod win32 {
    use std::os::windows::io::AsRawHandle;

    use super::*;
    use crate::win32::{HandleKind, SharedHandle};

    impl Dev {
        fn import_win32(
            &mut self,
            h: &SharedHandle<'_>,
            usage: ImageUsage,
        ) -> Result<Image, GpuError> {
            validate::create_image(&CreateImage::new(h.width, h.height, h.format, usage))?;
            let fns = self.ext.win32.clone().ok_or_else(|| {
                GpuError::Unsupported("this device does not import Windows handles".into())
            })?;
            self.retire();
            let handle_type = match h.kind {
                HandleKind::Memory => vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32,
                HandleKind::D3d11Texture => vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE,
                HandleKind::D3d12Resource => vk::ExternalMemoryHandleTypeFlags::D3D12_RESOURCE,
            };
            let vkf = vk_format(h.format);
            let (flags, _) = Self::image_usage(usage, h.format);
            let mut external =
                vk::ExternalMemoryImageCreateInfo::default().handle_types(handle_type);
            let info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vkf)
                .extent(vk::Extent3D {
                    width: h.width,
                    height: h.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(flags)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut external);
            let raw = h.handle.as_raw_handle() as vk::HANDLE;
            // SAFETY: valid create infos and a live handle, borrowed for the
            // calls (importing a Windows handle does not take it over);
            // everything made is destroyed on a later failure.
            let (image, memory, view) = unsafe {
                let image = self.device.create_image(&info, None).map_err(vk_error)?;
                let reqs = self.device.get_image_memory_requirements(image);
                let mut props = vk::MemoryWin32HandlePropertiesKHR::default();
                let typed = fns
                    .get_memory_win32_handle_properties(handle_type, raw, &mut props)
                    .map_err(vk_error)
                    .and_then(|()| {
                        first_type(reqs.memory_type_bits & props.memory_type_bits).ok_or_else(
                            || GpuError::Unsupported("no memory type imports this handle".into()),
                        )
                    });
                let index = match typed {
                    Ok(i) => i,
                    Err(e) => {
                        self.device.destroy_image(image, None);
                        return Err(e);
                    }
                };
                let mut import = vk::ImportMemoryWin32HandleInfoKHR::default()
                    .handle_type(handle_type)
                    .handle(raw);
                let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
                let alloc = vk::MemoryAllocateInfo::default()
                    .allocation_size(reqs.size)
                    .memory_type_index(index)
                    .push_next(&mut import)
                    .push_next(&mut dedicated);
                let memory = match self.device.allocate_memory(&alloc, None) {
                    Ok(m) => m,
                    Err(e) => {
                        self.device.destroy_image(image, None);
                        return Err(vk_error(e));
                    }
                };
                if let Err(e) = self.device.bind_image_memory(image, memory, 0) {
                    self.device.destroy_image(image, None);
                    self.device.free_memory(memory, None);
                    return Err(vk_error(e));
                }
                match self.view(
                    image,
                    &CreateImage::new(h.width, h.height, h.format, usage),
                    None,
                    false,
                ) {
                    Ok(v) => (image, memory, v),
                    Err(e) => {
                        self.device.destroy_image(image, None);
                        self.device.free_memory(memory, None);
                        return Err(e);
                    }
                }
            };
            let id = self.add(
                ResInfo::Image(CreateImage::new(h.width, h.height, h.format, usage)),
                Kind::image(
                    image,
                    memory,
                    view,
                    vk::ImageLayout::GENERAL,
                    Foreign::Acquire(vk::QUEUE_FAMILY_EXTERNAL),
                    h.format,
                ),
            );
            Ok(Image(id))
        }
    }

    now! {
        ['a, 'h] Opening<Import<&'a SharedHandle<'h>>> => Result<Image, GpuError>,
            |dev, Opening(op, _)| dev.import_win32(op.source, op.usage);
    }
}

// Apple systems: IOSurfaces, through MoltenVK.

#[cfg(target_vendor = "apple")]
mod metal {
    use super::*;
    use crate::metal::IoSurface;

    impl Dev {
        fn import_iosurface(
            &mut self,
            s: &IoSurface<'_>,
            usage: ImageUsage,
        ) -> Result<Image, GpuError> {
            validate::create_image(&CreateImage::new(s.width, s.height, s.format, usage))?;
            if !self.ext.metal {
                return Err(GpuError::Unsupported(
                    "this device does not import IOSurfaces (VK_EXT_metal_objects)".into(),
                ));
            }
            self.retire();
            let vkf = vk_format(s.format);
            let (flags, _) = Self::image_usage(usage, s.format);
            let mut surface =
                vk::ImportMetalIOSurfaceInfoEXT::default().io_surface(s.as_ptr().as_ptr().cast());
            let info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vkf)
                .extent(vk::Extent3D {
                    width: s.width,
                    height: s.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(flags)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut surface);
            // SAFETY: valid create infos and a live IOSurface (per
            // `IoSurface::from_raw`), which MoltenVK retains; everything
            // made is destroyed on a later failure.
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
                match self.view(
                    image,
                    &CreateImage::new(s.width, s.height, s.format, usage),
                    None,
                    false,
                ) {
                    Ok(v) => (image, memory, v),
                    Err(e) => {
                        self.device.destroy_image(image, None);
                        self.device.free_memory(memory, None);
                        return Err(e);
                    }
                }
            };
            let id = self.add(
                ResInfo::Image(CreateImage::new(s.width, s.height, s.format, usage)),
                Kind::image(
                    image,
                    memory,
                    view,
                    // Its contents are kept (Metal has no layouts to lose).
                    vk::ImageLayout::GENERAL,
                    Foreign::No,
                    s.format,
                ),
            );
            Ok(Image(id))
        }
    }

    now! {
        ['a, 'h] Opening<Import<&'a IoSurface<'h>>> => Result<Image, GpuError>,
            |dev, Opening(op, _)| dev.import_iosurface(op.source, op.usage);
    }
}
