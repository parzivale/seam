//! The simulated GPU: data movement, checks, emulators, clears.

use seam_ash::*;
use seam_core::prelude::*;

#[test]
fn data_moves_through_buffers_and_images() {
    let mut gpu = SimGpu::new();
    let (copied, pixels) = gpu
        .acquire(CreateBuffer::new(8, BufferUsage::STORAGE), |gpu, a| {
            gpu.acquire(CreateBuffer::new(8, BufferUsage::STORAGE), |gpu, b| {
                gpu.acquire(
                    CreateImage::new(2, 1, Format::Rgba8Unorm, ImageUsage::SAMPLED),
                    |gpu, img| {
                        gpu.perform(Upload::new(*a, &[1, 2, 3, 4]).at(4))?;
                        gpu.perform(CopyBuffer {
                            src: *a,
                            src_offset: 4,
                            dst: *b,
                            dst_offset: 0,
                            size: 4,
                        })?;
                        gpu.perform(UploadImage::new(*img, &[9; 8]))?;
                        let mut out = [0; 8];
                        gpu.perform(Download::new(*b, &mut out))?;
                        let mut px = [0; 8];
                        gpu.perform(DownloadImage::new(*img, &mut px))?;
                        Ok::<_, GpuError>((out, px))
                    },
                )
            })
        })
        .unwrap();
    assert_eq!(copied, [1, 2, 3, 4, 0, 0, 0, 0]);
    assert_eq!(pixels, [9; 8]);
    assert_eq!(gpu.live(), 0);
}

#[test]
fn draws_clear_their_target_and_run_their_emulator() {
    let mut gpu = SimGpu::new();
    gpu.emulate("dot", |run| {
        let (pixels, w, _) = run.target();
        // Paints the top-left pixel white.
        pixels[..4].copy_from_slice(&[255; 4]);
        assert_eq!(w, 2);
    });
    let kind = ProgramKind::Draw {
        vertex: &[],
        vertex_entry: "vs",
        fragment: &[],
        fragment_entry: "fs",
        attrs: &[],
        stride: 0,
        target: Format::Bgra8Unorm,
        blend: Blend::Replace,
    };
    let pixels = gpu
        .acquire(CreateProgram::draw("dot", kind, &[]), |gpu, program| {
            gpu.acquire(
                CreateImage::new(2, 2, Format::Bgra8Unorm, ImageUsage::TARGET),
                |gpu, target| {
                    gpu.perform(Draw::new(*program, *target, 3).clear([1.0, 0.0, 0.0, 1.0]))?;
                    let mut out = [0; 16];
                    gpu.perform(DownloadImage::new(*target, &mut out))?;
                    Ok::<_, GpuError>(out)
                },
            )
        })
        .unwrap();
    // Cleared to red, in BGRA order, except the painted pixel.
    assert_eq!(pixels[..4], [255; 4]);
    assert_eq!(pixels[4..8], [0, 0, 255, 255]);
    assert!(
        gpu.log()
            .iter()
            .any(|r| matches!(r, Recorded::Draw { label, .. } if label == "dot"))
    );
}

#[test]
fn mistakes_are_caught() {
    let mut gpu = SimGpu::new();
    let program = CreateProgram::compute("p", &[], "main", &[BindingKind::StorageBuffer]);
    gpu.acquire(program, |gpu, program| {
        gpu.acquire(CreateBuffer::new(4, BufferUsage::UNIFORM), |gpu, buf| {
            let wrong_kind =
                gpu.perform(Dispatch::new(*program, &[Bind::Uniform(*buf)], [1, 1, 1]));
            assert!(matches!(wrong_kind, Err(GpuError::Invalid(why)) if why.contains("binding 0")));
            let wrong_usage =
                gpu.perform(Dispatch::new(*program, &[Bind::Storage(*buf)], [1, 1, 1]));
            assert!(
                matches!(wrong_usage, Err(GpuError::Invalid(why)) if why.contains("not created"))
            );
            let push = gpu.perform(Dispatch::new(*program, &[], [1, 1, 1]));
            assert!(push.is_err());
            Ok::<_, GpuError>(*buf)
        })
    })
    .and_then(|gone| {
        // Its scope closed: the ID is dead.
        let mut out = [0; 4];
        gpu.perform(Download::new(gone, &mut out))
    })
    .map(drop)
    .expect_err("a buffer used after its scope fails");
    let zero = gpu.acquire(CreateBuffer::new(0, BufferUsage::STORAGE), |_, _| {
        Ok::<_, GpuError>(())
    });
    assert!(zero.is_err());
}

#[test]
fn async_cores_run_on_it_through_the_lifts() {
    let mut gpu = SimGpu::new();
    let out = seam_core::future::block_on(gpu.acquire_async(
        CreateBuffer::new(4, BufferUsage::STORAGE),
        async |gpu, buf| {
            gpu.perform_async(Upload::new(*buf, &[7, 7, 7, 7])).await?;
            let mut out = [0; 4];
            gpu.perform_async(Download::new(*buf, &mut out)).await?;
            Ok::<_, GpuError>(out)
        },
    ))
    .unwrap();
    assert_eq!(out, [7; 4]);
}

#[test]
fn submissions_on_the_simulation_are_done_at_once() {
    let mut gpu = SimGpu::new();
    gpu.acquire(Submit::<SimSubmission>::new(), |_, submission| {
        assert!(submission.inspect(IsDone)?);
        submission.perform(Wait)
    })
    .unwrap();
    assert!(gpu.log().iter().any(|r| matches!(r, Recorded::Submit)));
}
