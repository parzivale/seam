//! The checks every handler makes before an operation, so that a mistake
//! fails the same way on [`SimGpu`](crate::SimGpu) as on real Vulkan, and
//! never reaches the driver.

use std::format;

use crate::{
    Bind, BindingKind, BufferUsage, CopyBuffer, CreateBuffer, CreateImage, CreateProgram, Dispatch,
    Draw, GpuError, ImageUsage, ProgramKind, ResInfo, invalid,
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

pub(crate) fn image(
    l: &impl Lookup,
    id: u64,
) -> Result<(u32, u32, crate::Format, ImageUsage), GpuError> {
    match l.info(id) {
        Some(ResInfo::Image {
            width,
            height,
            format,
            usage,
        }) => Ok((*width, *height, *format, *usage)),
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
    pub target: Option<crate::Format>,
    pub stride: u32,
}

pub(crate) fn program(l: &impl Lookup, id: u64) -> Result<ProgramInfo<'_>, GpuError> {
    match l.info(id) {
        Some(ResInfo::Program {
            label,
            compute,
            bindings,
            push_constants,
            target,
            stride,
        }) => Ok(ProgramInfo {
            label,
            compute: *compute,
            bindings,
            push_constants: *push_constants,
            target: *target,
            stride: *stride,
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

pub(crate) fn create_image(op: &CreateImage) -> Result<(), GpuError> {
    if op.width == 0 || op.height == 0 {
        return Err(invalid("an image needs at least one pixel"));
    }
    Ok(())
}

pub(crate) fn create_program(op: &CreateProgram<'_>) -> Result<(), GpuError> {
    if !op.push_constants.is_multiple_of(4) || op.push_constants > 128 {
        return Err(invalid(format!(
            "program `{}`: push constants must be a multiple of 4 bytes, at most 128",
            op.label
        )));
    }
    if let ProgramKind::Draw { attrs, stride, .. } = op.kind
        && !attrs.is_empty()
        && stride == 0
    {
        return Err(invalid(format!(
            "program `{}`: vertex attributes need a stride",
            op.label
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

/// The byte size of a whole image.
pub(crate) fn image_bytes(l: &impl Lookup, id: u64) -> Result<u64, GpuError> {
    let (w, h, format, _) = image(l, id)?;
    Ok(u64::from(w) * u64::from(h) * u64::from(format.bytes_per_pixel()))
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
                image(l, img.raw())?.3.contains(ImageUsage::SAMPLED)
            }
            Bind::Texture(img) => image(l, img.raw())?.3.contains(ImageUsage::SAMPLED),
            Bind::Sampler(smp) => sampler(l, smp.raw()).is_ok(),
            Bind::StorageImage(img) => image(l, img.raw())?.3.contains(ImageUsage::STORAGE),
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

pub(crate) fn draw(l: &impl Lookup, op: &Draw<'_>) -> Result<(), GpuError> {
    let p = program(l, op.program.raw())?;
    let Some(target_format) = p.target else {
        return Err(invalid(format!(
            "program `{}` is not a draw program",
            p.label
        )));
    };
    let (_, _, format, usage) = image(l, op.target.raw())?;
    if !usage.contains(ImageUsage::TARGET) {
        return Err(invalid(
            "draw: the target image was not created as a target",
        ));
    }
    if format != target_format {
        return Err(invalid(format!(
            "draw: program `{}` draws {target_format:?}, the target is {format:?}",
            p.label
        )));
    }
    match op.vertices {
        Some(v) => {
            let (size, usage) = buffer(l, v.raw())?;
            if !usage.contains(BufferUsage::VERTEX) {
                return Err(invalid(
                    "draw: the vertex buffer was not created for vertices",
                ));
            }
            let needed = u64::from(p.stride) * u64::from(op.vertex_count);
            if needed > size {
                return Err(invalid(format!(
                    "draw: {} vertices need {needed} bytes, the buffer has {size}",
                    op.vertex_count
                )));
            }
        }
        None if p.stride != 0 => {
            return Err(invalid(format!(
                "draw: program `{}` reads vertices, none were given",
                p.label
            )));
        }
        None => {}
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
