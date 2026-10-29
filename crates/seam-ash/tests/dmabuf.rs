//! Importing dmabufs. Real ones come from a GPU driver; these are memfds
//! holding linear pixels, which llvmpipe imports as dmabufs and `SimGpu`
//! reads. One core runs on both.
#![cfg(target_os = "linux")]

use seam_ash::drm::{Dmabuf, Fourcc, Modifier, Plane};
use seam_ash::*;
use seam_core::prelude::*;

/// A fake dmabuf: `rows` written `stride` bytes apart, after `offset`.
fn memfd_dmabuf(width: u32, format: Fourcc, rows: &[&[u8]], offset: u32, stride: u32) -> Dmabuf {
    use std::io::Write;
    let fd = rustix::fs::memfd_create("fake dmabuf", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    let mut file = std::fs::File::from(fd);
    let mut bytes = vec![0xee; offset as usize + stride as usize * rows.len()];
    for (y, row) in rows.iter().enumerate() {
        let at = offset as usize + y * stride as usize;
        bytes[at..at + row.len()].copy_from_slice(row);
    }
    file.write_all(&bytes).unwrap();
    Dmabuf {
        width,
        height: rows.len() as u32,
        format,
        modifier: Modifier::LINEAR,
        planes: vec![Plane {
            fd: file.into(),
            offset,
            stride,
        }],
    }
}

/// The core: imports a client's buffer and reads it back.
fn read_back<G>(gpu: &mut G, buf: &Dmabuf) -> Result<Vec<u8>, GpuError>
where
    G: for<'a> Provide<Import<&'a Dmabuf>> + for<'a> Perform<DownloadImage<'a>>,
{
    gpu.acquire(Import::new(buf), |gpu, image| {
        let mut out = vec![0; (buf.width * buf.height * 4) as usize];
        gpu.perform(DownloadImage::new(*image, &mut out))?;
        Ok(out)
    })
}

/// Two rows of two pixels, padded to 12-byte rows, after 4 bytes.
fn client_buffer() -> Dmabuf {
    memfd_dmabuf(
        2,
        Fourcc::ARGB8888,
        &[&[1, 2, 3, 4, 5, 6, 7, 8], &[9, 10, 11, 12, 13, 14, 15, 16]],
        4,
        12,
    )
}

const PIXELS: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

#[test]
fn the_simulated_gpu_reads_a_dmabuf() {
    let mut sim = SimGpu::new();
    assert_eq!(read_back(&mut sim, &client_buffer()).unwrap(), PIXELS);
    assert!(sim.log().iter().any(|r| matches!(r, Recorded::Import(_))));
    assert_eq!(sim.live(), 0);
    // An unused alpha reads as opaque.
    let opaque = memfd_dmabuf(1, Fourcc::XRGB8888, &[&[1, 2, 3, 0]], 0, 4);
    assert_eq!(read_back(&mut sim, &opaque).unwrap(), [1, 2, 3, 255]);
}

#[test]
fn mistakes_in_a_dmabuf_are_caught() {
    let mut sim = SimGpu::new();
    let mut short = client_buffer();
    short.planes[0].stride = 4;
    assert!(
        matches!(read_back(&mut sim, &short), Err(GpuError::Invalid(why)) if why.contains("stride"))
    );
    let mut odd = client_buffer();
    odd.format = Fourcc::from_chars(*b"NV12");
    assert!(matches!(
        read_back(&mut sim, &odd),
        Err(GpuError::Unsupported(_))
    ));
    let mut tiled = client_buffer();
    tiled.modifier = Modifier(0x0100_0000_0000_0001);
    assert!(matches!(
        read_back(&mut sim, &tiled),
        Err(GpuError::Unsupported(_))
    ));
}

#[cfg(feature = "ash")]
mod vulkan {
    use super::*;
    use seam_ash::drm::DmabufFormats;

    fn gpu() -> Option<Ash> {
        Ash::with(&AshOptions {
            validation: true,
            ..AshOptions::default()
        })
        .map_err(|e| eprintln!("skipping: no Vulkan device with validation here ({e})"))
        .ok()
    }

    fn clean(gpu: &Ash) {
        let messages = gpu.validation_messages();
        assert!(
            messages.is_empty(),
            "validation reported:\n{}",
            messages.join("\n\n")
        );
    }

    /// Skips where the device imports no dmabufs.
    fn imports(gpu: &Ash) -> bool {
        let formats = gpu.inspect(DmabufFormats).unwrap();
        if formats.is_empty() {
            eprintln!("skipping: this device imports no dmabufs");
        }
        !formats.is_empty()
    }

    #[test]
    fn a_dmabuf_on_vulkan_reads_as_on_the_simulation() {
        let Some(mut gpu) = gpu() else { return };
        if !imports(&gpu) {
            return;
        }
        let formats = gpu.inspect(DmabufFormats).unwrap();
        assert!(
            formats
                .iter()
                .any(|f| f.fourcc == Fourcc::ARGB8888 && f.modifier == Modifier::LINEAR)
        );
        let buf = client_buffer();
        assert_eq!(read_back(&mut gpu, &buf).unwrap(), PIXELS);
        assert_eq!(read_back(&mut SimGpu::new(), &buf).unwrap(), PIXELS);
        // Imported again, after the client changed it.
        let changed = memfd_dmabuf(2, Fourcc::ARGB8888, &[&[9; 8], &[7; 8]], 4, 12);
        let mut want = [9; 16];
        want[8..].fill(7);
        assert_eq!(read_back(&mut gpu, &changed).unwrap(), want);
        assert_eq!(gpu.pending_counts(), (0, 0));
        clean(&gpu);
    }

    #[test]
    fn unimportable_layouts_say_so() {
        let Some(mut gpu) = gpu() else { return };
        if !imports(&gpu) {
            return;
        }
        let mut tiled = client_buffer();
        tiled.modifier = Modifier(0x0100_0000_0000_0001);
        let supported = gpu
            .inspect(DmabufFormats)
            .unwrap()
            .iter()
            .any(|f| f.modifier == tiled.modifier);
        if !supported {
            assert!(matches!(
                read_back(&mut gpu, &tiled),
                Err(GpuError::Unsupported(_))
            ));
        }
        clean(&gpu);
    }

    /// Imported on a multithreaded runtime, while drawing nothing else:
    /// the async form, through the scope.
    #[test]
    fn async_imports_on_tokio() {
        let Some(gpu) = gpu() else { return };
        if !imports(&gpu) {
            return;
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .unwrap();
        let out = runtime.block_on(async {
            let mut gpu = gpu.clone();
            let buf = client_buffer();
            tokio::spawn(async move {
                gpu.acquire_send(Import::new(&buf), async |gpu, image| {
                    let mut out = vec![0; 16];
                    gpu.perform_send(DownloadImage::new(*image, &mut out))
                        .await?;
                    Ok::<_, GpuError>(out)
                })
                .await
            })
            .await
            .unwrap()
        });
        assert_eq!(out.unwrap(), PIXELS);
        clean(&gpu);
    }
}
