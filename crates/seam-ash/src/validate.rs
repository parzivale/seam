//! The checks every handler makes before an operation, so that a mistake
//! fails the same way on [`SimGpu`](crate::SimGpu) as on real Vulkan, and
//! never reaches the driver.

use std::format;

use crate::{
    Bind, BindingKind, BufferUsage, CopyBuffer, CreateBuffer, CreateImage, CreateProgram,
    DepthLoad, Dispatch, Draw, DrawInfo, Format, GpuError, ImageKind, ImageUsage, ProgramKind,
    ResInfo, Step, Topology, invalid,
};

/// Looks resources up by ID.
pub(crate) trait Lookup {
    fn info(&self, id: u64) -> Option<&ResInfo>;
}

fn gone(what: &str, id: u64) -> GpuError {
    invalid(format!(
        "{what} {id} does not exist (or its scope has closed)"
    ))
}

pub(crate) fn buffer(l: &impl Lookup, id: u64) -> Result<(u64, BufferUsage), GpuError> {
    match l.info(id) {
        Some(ResInfo::Buffer { size, usage }) => Ok((*size, *usage)),
        _ => Err(gone("buffer", id)),
    }
}

pub(crate) fn image(l: &impl Lookup, id: u64) -> Result<CreateImage, GpuError> {
    match l.info(id) {
        Some(ResInfo::Image(op)) => Ok(*op),
        _ => Err(gone("image", id)),
    }
}

pub(crate) fn sampler(l: &impl Lookup, id: u64) -> Result<crate::CreateSampler, GpuError> {
    match l.info(id) {
        Some(ResInfo::Sampler(s)) => Ok(*s),
        _ => Err(gone("sampler", id)),
    }
}

pub(crate) struct ProgramInfo<'a> {
    pub label: &'a str,
    pub compute: bool,
    pub bindings: &'a [BindingKind],
    pub push_constants: u32,
    pub draw: Option<&'a DrawInfo>,
}

pub(crate) fn program(l: &impl Lookup, id: u64) -> Result<ProgramInfo<'_>, GpuError> {
    match l.info(id) {
        Some(ResInfo::Program {
            label,
            compute,
            bindings,
            push_constants,
            draw,
        }) => Ok(ProgramInfo {
            label,
            compute: *compute,
            bindings,
            push_constants: *push_constants,
            draw: draw.as_ref(),
        }),
        _ => Err(gone("program", id)),
    }
}

fn range(what: &str, offset: u64, len: u64, size: u64) -> Result<(), GpuError> {
    match offset.checked_add(len) {
        Some(end) if end <= size => Ok(()),
        _ => Err(invalid(format!(
            "{what}: bytes {offset}..{} are outside the buffer's {size}",
            offset.saturating_add(len)
        ))),
    }
}

pub(crate) fn create_buffer(op: &CreateBuffer) -> Result<(), GpuError> {
    if op.size == 0 {
        return Err(invalid("a buffer needs at least one byte"));
    }
    Ok(())
}

fn valid_samples(n: u32) -> bool {
    n.is_power_of_two() && n <= 64
}

pub(crate) fn create_image(op: &CreateImage) -> Result<(), GpuError> {
    if op.width == 0 || op.height == 0 || op.depth == 0 || op.layers == 0 {
        return Err(invalid("an image needs at least one pixel and one layer"));
    }
    let (w, h, d) = op.extent(0);
    let largest = w.max(h).max(d);
    if op.mips == 0 || op.mips > 32 - largest.leading_zeros() {
        return Err(invalid(format!(
            "a {w}x{h}x{d} image has 1 to {} mip levels, not {}",
            32 - largest.leading_zeros(),
            op.mips
        )));
    }
    if !valid_samples(op.samples) {
        return Err(invalid(format!(
            "samples are a power of two, at most 64, not {}",
            op.samples
        )));
    }
    if op.samples > 1 {
        if op.mips > 1 || op.kind != ImageKind::D2 {
            return Err(invalid("a multisampled image is flat, with one mip level"));
        }
        if !op.usage.contains(ImageUsage::TARGET) {
            return Err(invalid("a multisampled image is a draw target"));
        }
    }
    match op.kind {
        ImageKind::D2 if op.depth != 1 => {
            return Err(invalid("a flat image has a depth of 1: use ImageKind::D3"));
        }
        ImageKind::Cube if op.width != op.height || !op.layers.is_multiple_of(6) => {
            return Err(invalid(
                "a cube's faces are square, and it has six layers per cube",
            ));
        }
        ImageKind::Cube if op.depth != 1 => {
            return Err(invalid("a cube has a depth of 1"));
        }
        ImageKind::D3 if op.layers != 1 => {
            return Err(invalid("a volume has one layer"));
        }
        _ => {}
    }
    if op.format.is_compressed() && op.usage.contains(ImageUsage::TARGET) {
        return Err(invalid(format!(
            "{:?} images cannot be drawn into",
            op.format
        )));
    }
    if op.format.is_compressed() && op.usage.contains(ImageUsage::STORAGE) {
        return Err(invalid(format!(
            "{:?} images cannot be storage images",
            op.format
        )));
    }
    if op.format.is_depth() && op.usage.contains(ImageUsage::STORAGE) {
        return Err(invalid("depth images cannot be storage images"));
    }
    if op.format.is_depth() && op.kind == ImageKind::D3 {
        return Err(invalid("depth images are flat or cubes"));
    }
    Ok(())
}

pub(crate) fn create_program(op: &CreateProgram<'_>) -> Result<(), GpuError> {
    let label = op.label;
    if !op.push_constants.is_multiple_of(4) || op.push_constants > 128 {
        return Err(invalid(format!(
            "program `{label}`: push constants must be a multiple of 4 bytes, at most 128"
        )));
    }
    let ProgramKind::Draw(p) = op.kind else {
        return Ok(());
    };
    let mut locations = Vec::new();
    for (slot, layout) in p.vertex_buffers.iter().enumerate() {
        let Some(layout) = layout else { continue };
        if layout.stride == 0 {
            return Err(invalid(format!(
                "program `{label}`: vertex buffer {slot} needs a stride"
            )));
        }
        for a in layout.attrs {
            if a.offset + a.format.size() > layout.stride {
                return Err(invalid(format!(
                    "program `{label}`: the attribute at location {} runs past vertex buffer {slot}'s stride",
                    a.location
                )));
            }
            if locations.contains(&a.location) {
                return Err(invalid(format!(
                    "program `{label}`: two attributes at location {}",
                    a.location
                )));
            }
            locations.push(a.location);
        }
    }
    if p.targets.iter().all(Option::is_none) && p.depth.is_none() {
        return Err(invalid(format!(
            "program `{label}` draws into nothing: give it a colour target or depth"
        )));
    }
    for t in p.targets.iter().flatten() {
        if t.format.is_depth() || t.format.is_compressed() {
            return Err(invalid(format!(
                "program `{label}`: {:?} is not a colour target format",
                t.format
            )));
        }
        if t.format.is_integer() && t.blend != crate::Blend::Replace {
            return Err(invalid(format!(
                "program `{label}`: integer targets ({:?}) cannot blend",
                t.format
            )));
        }
    }
    if let Some(d) = p.depth {
        if !d.format.is_depth() {
            return Err(invalid(format!(
                "program `{label}`: {:?} is not a depth format",
                d.format
            )));
        }
        if d.stencil.is_some() && !d.format.has_stencil() {
            return Err(invalid(format!(
                "program `{label}`: a stencil test needs a format with stencil, not {:?}",
                d.format
            )));
        }
    }
    if !valid_samples(p.samples) {
        return Err(invalid(format!(
            "program `{label}`: samples are a power of two, at most 64, not {}",
            p.samples
        )));
    }
    if p.raster.restart
        && !matches!(
            p.raster.topology,
            Topology::LineStrip | Topology::TriangleStrip | Topology::TriangleFan
        )
    {
        return Err(invalid(format!(
            "program `{label}`: primitive restart is for strips and fans"
        )));
    }
    Ok(())
}

pub(crate) fn upload(l: &impl Lookup, id: u64, offset: u64, len: u64) -> Result<(), GpuError> {
    let (size, _) = buffer(l, id)?;
    range("upload", offset, len, size)
}

pub(crate) fn download(l: &impl Lookup, id: u64, offset: u64, len: u64) -> Result<(), GpuError> {
    let (size, _) = buffer(l, id)?;
    range("download", offset, len, size)
}

/// The byte size of a whole image's level 0, every layer.
pub(crate) fn image_bytes(l: &impl Lookup, id: u64) -> Result<u64, GpuError> {
    let op = image(l, id)?;
    if op.samples > 1 {
        return Err(invalid(
            "a multisampled image cannot be moved in or out: resolve it first",
        ));
    }
    if op.format.has_stencil() {
        return Err(invalid(format!(
            "{:?} images cannot be moved in or out (D16Unorm and D32Float can)",
            op.format
        )));
    }
    Ok(op.level_bytes(0))
}

pub(crate) fn whole_image(
    l: &impl Lookup,
    id: u64,
    len: usize,
    what: &str,
) -> Result<(), GpuError> {
    let want = image_bytes(l, id)?;
    if len as u64 != want {
        return Err(invalid(format!(
            "{what}: the image is {want} bytes, the data {len}"
        )));
    }
    Ok(())
}

pub(crate) fn copy(l: &impl Lookup, op: &CopyBuffer) -> Result<(), GpuError> {
    let (src_size, _) = buffer(l, op.src.raw())?;
    let (dst_size, _) = buffer(l, op.dst.raw())?;
    range("copy from", op.src_offset, op.size, src_size)?;
    range("copy to", op.dst_offset, op.size, dst_size)?;
    if op.src == op.dst {
        let (a, b) = (op.src_offset, op.dst_offset);
        if a < b + op.size && b < a + op.size {
            return Err(invalid("copy: the ranges overlap"));
        }
    }
    Ok(())
}

/// Checks bindings against a program's layout, and each resource's usage.
fn bindings(
    l: &impl Lookup,
    p: &ProgramInfo<'_>,
    bound: &[Bind],
    push: &[u8],
) -> Result<(), GpuError> {
    if bound.len() != p.bindings.len() {
        return Err(invalid(format!(
            "program `{}` has {} bindings, {} were given",
            p.label,
            p.bindings.len(),
            bound.len()
        )));
    }
    for (i, (b, want)) in bound.iter().zip(p.bindings).enumerate() {
        if b.kind() != *want {
            return Err(invalid(format!(
                "program `{}`: binding {i} is a {want:?}, given a {:?}",
                p.label,
                b.kind()
            )));
        }
        let ok = match *b {
            Bind::Uniform(buf) => buffer(l, buf.raw())?.1.contains(BufferUsage::UNIFORM),
            Bind::Storage(buf) => buffer(l, buf.raw())?.1.contains(BufferUsage::STORAGE),
            Bind::Sampled(img, smp) => {
                sampler(l, smp.raw())?;
                image(l, img.raw())?.usage.contains(ImageUsage::SAMPLED)
            }
            Bind::Texture(img) => image(l, img.raw())?.usage.contains(ImageUsage::SAMPLED),
            Bind::Sampler(smp) => sampler(l, smp.raw()).is_ok(),
            Bind::StorageImage(img) => image(l, img.raw())?.usage.contains(ImageUsage::STORAGE),
        };
        if !ok {
            return Err(invalid(format!(
                "program `{}`: binding {i}'s resource was not created for that use",
                p.label
            )));
        }
    }
    if push.len() as u32 != p.push_constants {
        return Err(invalid(format!(
            "program `{}` takes {} bytes of push constants, {} were given",
            p.label,
            p.push_constants,
            push.len()
        )));
    }
    Ok(())
}

pub(crate) fn dispatch(l: &impl Lookup, op: &Dispatch<'_>) -> Result<(), GpuError> {
    let p = program(l, op.program.raw())?;
    if !p.compute {
        return Err(invalid(format!(
            "program `{}` is not a compute program",
            p.label
        )));
    }
    bindings(l, &p, op.bindings, op.push)
}

/// One image a draw writes, checked against what the program draws.
fn attachment(
    l: &impl Lookup,
    label: &str,
    what: &str,
    (id, mip, layer): (u64, u32, u32),
    format: Format,
    samples: u32,
) -> Result<CreateImage, GpuError> {
    let img = image(l, id)?;
    if !img.usage.contains(ImageUsage::TARGET) {
        return Err(invalid(format!("draw: {what} was not created as a target")));
    }
    if img.format != format {
        return Err(invalid(format!(
            "draw: program `{label}` draws {format:?} into {what}, which is {:?}",
            img.format
        )));
    }
    if img.samples != samples {
        return Err(invalid(format!(
            "draw: program `{label}` draws {samples} samples per pixel, {what} has {}",
            img.samples
        )));
    }
    let layers = if img.kind == ImageKind::D3 {
        img.extent(mip).2
    } else {
        img.layers
    };
    if mip >= img.mips || layer >= layers {
        return Err(invalid(format!(
            "draw: {what} has no mip level {mip}, layer {layer}"
        )));
    }
    Ok(img)
}

pub(crate) fn draw(l: &impl Lookup, op: &Draw<'_>) -> Result<(), GpuError> {
    let p = program(l, op.program.raw())?;
    let Some(info) = p.draw else {
        return Err(invalid(format!(
            "program `{}` is not a draw program",
            p.label
        )));
    };
    let label = p.label;
    let mut size: Option<(u32, u32)> = None;
    let mut same_size = |what: &str, img: &CreateImage, mip: u32| -> Result<(), GpuError> {
        let (w, h, _) = img.extent(mip);
        match size {
            None => {
                size = Some((w, h));
                Ok(())
            }
            Some(s) if s == (w, h) => Ok(()),
            Some((sw, sh)) => Err(invalid(format!(
                "draw: {what} is {w}x{h}, the other targets {sw}x{sh}"
            ))),
        }
    };
    let mut written = Vec::new();
    for (slot, (want, got)) in info.targets.iter().zip(&op.targets).enumerate() {
        let what = format!("target {slot}");
        match (want, got) {
            (None, None) => {}
            (None, Some(_)) => {
                return Err(invalid(format!(
                    "draw: program `{label}` has no target {slot}"
                )));
            }
            (Some(_), None) => {
                return Err(invalid(format!(
                    "draw: program `{label}` draws into target {slot}, none was given"
                )));
            }
            (Some(format), Some(a)) => {
                let img = attachment(
                    l,
                    label,
                    &what,
                    (a.image.raw(), a.mip, a.layer),
                    *format,
                    info.samples,
                )?;
                same_size(&what, &img, a.mip)?;
                written.push(a.image.raw());
                if let Some(r) = a.resolve {
                    if info.samples == 1 {
                        return Err(invalid(format!(
                            "draw: {what} is not multisampled, so has nothing to resolve"
                        )));
                    }
                    if format.is_integer() {
                        return Err(invalid(format!(
                            "draw: {what} holds integers, which do not resolve"
                        )));
                    }
                    let into = image(l, r.raw())?;
                    if into.samples != 1
                        || into.format != *format
                        || into.extent(0).0 != img.extent(a.mip).0
                        || into.extent(0).1 != img.extent(a.mip).1
                    {
                        return Err(invalid(format!(
                            "draw: {what} resolves into an image of its format and size, with one sample"
                        )));
                    }
                    if !into.usage.contains(ImageUsage::TARGET) {
                        return Err(invalid(format!(
                            "draw: {what}'s resolve image was not created as a target"
                        )));
                    }
                    written.push(r.raw());
                }
            }
        }
    }
    match (info.depth, op.depth) {
        (None, None) => {}
        (None, Some(_)) => {
            return Err(invalid(format!(
                "draw: program `{label}` does not test depth, a depth image was given"
            )));
        }
        (Some(_), None) => {
            return Err(invalid(format!(
                "draw: program `{label}` tests depth, no depth image was given"
            )));
        }
        (Some(format), Some(d)) => {
            let img = attachment(
                l,
                label,
                "the depth image",
                (d.image.raw(), d.mip, d.layer),
                format,
                info.samples,
            )?;
            same_size("the depth image", &img, d.mip)?;
            written.push(d.image.raw());
            if let DepthLoad::Clear { depth, .. } = d.load
                && !(0.0..=1.0).contains(&depth)
            {
                return Err(invalid("draw: depth clears to a value in [0, 1]"));
            }
        }
    }
    for i in 0..written.len() {
        if written[..i].contains(&written[i]) {
            return Err(invalid("draw: an image is written twice"));
        }
    }
    for b in op.bindings {
        let id = match *b {
            Bind::Sampled(i, _) | Bind::Texture(i) | Bind::StorageImage(i) => i.raw(),
            Bind::Uniform(_) | Bind::Storage(_) | Bind::Sampler(_) => continue,
        };
        if written.contains(&id) {
            return Err(invalid("draw: an image it draws into is bound to it too"));
        }
    }
    let vertices = u64::from(op.vertex_count);
    let instances = u64::from(op.instance_count);
    for (slot, (want, got)) in info
        .vertex_buffers
        .iter()
        .zip(&op.vertex_buffers)
        .enumerate()
    {
        match (want, got) {
            (None, None) => {}
            (None, Some(_)) => {
                return Err(invalid(format!(
                    "draw: program `{label}` reads no vertex buffer {slot}"
                )));
            }
            (Some(_), None) => {
                return Err(invalid(format!(
                    "draw: program `{label}` reads vertex buffer {slot}, none was given"
                )));
            }
            (Some((stride, step)), Some(v)) => {
                let (size, usage) = buffer(l, v.buffer.raw())?;
                if !usage.contains(BufferUsage::VERTEX) {
                    return Err(invalid(format!(
                        "draw: vertex buffer {slot} was not created for vertices"
                    )));
                }
                // Indexed draws read where the indices say: unchecked.
                let elements = match step {
                    Step::Vertex if op.index.is_some() => 0,
                    Step::Vertex => u64::from(op.first) + vertices,
                    Step::Instance => u64::from(op.first_instance) + instances,
                };
                let needed = v.offset + u64::from(*stride) * elements;
                if (vertices > 0 && instances > 0) && needed > size {
                    return Err(invalid(format!(
                        "draw: vertex buffer {slot} needs {needed} bytes, it has {size}"
                    )));
                }
            }
        }
    }
    if let Some(index) = op.index {
        let (size, usage) = buffer(l, index.buffer.raw())?;
        if !usage.contains(BufferUsage::INDEX) {
            return Err(invalid(
                "draw: the index buffer was not created for indices",
            ));
        }
        if !index.offset.is_multiple_of(index.format.size()) {
            return Err(invalid(
                "draw: the index buffer's offset is not a multiple of the index size",
            ));
        }
        let needed = index.offset + index.format.size() * (u64::from(op.first) + vertices);
        if needed > size {
            return Err(invalid(format!(
                "draw: {} indices need {needed} bytes, the buffer has {size}",
                op.vertex_count
            )));
        }
    } else if op.base_vertex != 0 {
        return Err(invalid("draw: a base vertex is for indexed draws"));
    }
    if let Some(v) = op.viewport
        && (v.width <= 0.0
            || v.height == 0.0
            || !(0.0..=1.0).contains(&v.min_depth)
            || !(0.0..=1.0).contains(&v.max_depth))
    {
        return Err(invalid(
            "draw: a viewport has a positive width, a height, and depths in [0, 1]",
        ));
    }
    if let Some(s) = op.scissor
        && (s.x < 0 || s.y < 0)
    {
        return Err(invalid("draw: a scissor starts at x, y of 0 or more"));
    }
    bindings(l, &p, op.bindings, op.push)
}

/// A dmabuf to import: a format this crate reads, one plane, and rows at
/// least an image's width apart, a whole number of pixels.
#[cfg(drm)]
pub(crate) fn dmabuf(buf: &crate::drm::Dmabuf, usage: ImageUsage) -> Result<(), GpuError> {
    let Some((format, _)) = buf.format.format() else {
        return Err(GpuError::Unsupported(format!(
            "dmabufs in {:?}; see Fourcc::SUPPORTED",
            buf.format
        )));
    };
    create_image(&CreateImage::new(buf.width, buf.height, format, usage))?;
    let [plane] = buf.planes.as_slice() else {
        return Err(GpuError::Invalid(format!(
            "a {:?} dmabuf has one plane, not {}",
            buf.format,
            buf.planes.len()
        )));
    };
    let bpp = format.bytes_per_pixel();
    if u64::from(plane.stride) < u64::from(buf.width) * u64::from(bpp) {
        return Err(GpuError::Invalid(format!(
            "a dmabuf's stride of {} is shorter than a row of {} pixels",
            plane.stride, buf.width
        )));
    }
    if !plane.stride.is_multiple_of(bpp) {
        return Err(GpuError::Invalid(format!(
            "a dmabuf's stride of {} is not a whole number of {bpp}-byte pixels",
            plane.stride
        )));
    }
    Ok(())
}
