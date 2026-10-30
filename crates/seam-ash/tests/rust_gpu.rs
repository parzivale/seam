//! Shaders written in Rust (`shaders/`, compiled by rust-gpu): their
//! SPIR-V on Vulkan, and the same functions in `SimGpu`'s emulators.
#![cfg(feature = "ash")]

use seam_ash::*;
use seam_ash_shaders as shaders;
use seam_core::prelude::*;
use shaders::spirv_std::glam::{UVec3, vec2};

/// The shaders' SPIR-V, as rust-gpu wrote it.
fn spirv() -> Vec<u32> {
    bytes_u32(include_bytes!("../shaders/shaders.spv"))
}

fn gpu() -> Option<Ash> {
    match Ash::with(&AshOptions {
        validation: true,
        ..AshOptions::default()
    }) {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            eprintln!("skipping the Vulkan half: no device with validation ({e})");
            None
        }
    }
}

fn clean(gpu: &Ash) {
    let messages = gpu.validation_messages();
    assert!(
        messages.is_empty(),
        "validation reported:\n{}",
        messages.join("\n\n")
    );
}

/// The core: doubles numbers with the `double` shader.
fn double<G>(gpu: &mut G, spirv: &[u32], numbers: &[u32]) -> Result<Vec<u32>, GpuError>
where
    G: Provide<CreateBuffer>
        + for<'a> Provide<CreateProgram<'a>>
        + for<'a> Perform<Upload<'a>>
        + for<'a> Perform<Dispatch<'a>>
        + for<'a> Perform<Download<'a>>,
{
    let program = CreateProgram::compute("double", spirv, "double", &[BindingKind::StorageBuffer]);
    gpu.acquire(program, |gpu, program| {
        let bytes = u32_bytes(numbers);
        gpu.acquire(
            CreateBuffer::new(bytes.len() as u64, BufferUsage::STORAGE),
            |gpu, buf| {
                gpu.perform(Upload::new(*buf, &bytes))?;
                let groups = (numbers.len() as u32).div_ceil(shaders::THREADS);
                gpu.perform(Dispatch::new(
                    *program,
                    &[Bind::Storage(*buf)],
                    [groups, 1, 1],
                ))?;
                let mut out = vec![0; bytes.len()];
                gpu.perform(Download::new(*buf, &mut out))?;
                Ok(bytes_u32(&out))
            },
        )
    })
}

#[test]
fn a_rust_compute_shader_on_the_gpu_and_in_the_simulation() {
    let numbers: Vec<u32> = (0..100).collect();
    let doubled: Vec<u32> = numbers.iter().map(|n| n * 2).collect();

    // In the simulation, the emulator is the shader itself, invocation by
    // invocation.
    let mut sim = SimGpu::new();
    sim.emulate("double", |run| {
        let mut numbers = bytes_u32(run.buffer(0));
        for x in 0..run.groups()[0] * shaders::THREADS {
            shaders::double(UVec3::new(x, 0, 0), &mut numbers);
        }
        run.buffer(0).copy_from_slice(&u32_bytes(&numbers));
    });
    assert_eq!(double(&mut sim, &[], &numbers).unwrap(), doubled);

    let Some(mut gpu) = gpu() else { return };
    assert_eq!(double(&mut gpu, &spirv(), &numbers).unwrap(), doubled);
    clean(&gpu);
}

/// The core: draws what the `heights` vertex shader reads from a 2x1
/// heightmap at `uv`, and returns the first pixel.
fn height_at<G>(gpu: &mut G, spirv: &[u32], uv: [f32; 2]) -> Result<[u8; 4], GpuError>
where
    G: Provide<CreateImage>
        + Provide<CreateSampler>
        + for<'a> Provide<CreateProgram<'a>>
        + for<'a> Perform<UploadImage<'a>>
        + for<'a> Perform<Draw<'a>>
        + for<'a> Perform<DownloadImage<'a>>,
{
    let kind = ProgramKind::Draw(DrawPipeline::new(
        spirv,
        "heights_vs",
        spirv,
        "heights_fs",
        Format::Rgba8Unorm,
    ));
    let program =
        CreateProgram::draw("heights", kind, &[BindingKind::SampledImage]).push_constants(8);
    gpu.acquire(program, |gpu, program| {
        let map = CreateImage::new(2, 1, Format::R8Unorm, ImageUsage::SAMPLED);
        gpu.acquire(map, |gpu, map| {
            gpu.perform(UploadImage::new(*map, &[0, 200]))?;
            gpu.acquire(CreateSampler::new(Filter::Linear), |gpu, smp| {
                let target = CreateImage::new(1, 1, Format::Rgba8Unorm, ImageUsage::TARGET);
                gpu.acquire(target, |gpu, target| {
                    let push: Vec<u8> = uv.iter().flat_map(|c| c.to_le_bytes()).collect();
                    let bindings = [Bind::Sampled(*map, *smp)];
                    gpu.perform(Draw::new(*program, *target, 3).bind(&bindings).push(&push))?;
                    let mut px = [0; 4];
                    gpu.perform(DownloadImage::new(*target, &mut px))?;
                    Ok(px)
                })
            })
        })
    })
}

#[test]
fn a_rust_vertex_shader_samples_a_heightmap() {
    // Halfway between the texels: linear filtering gives 100.
    let uv = [0.5, 0.5];
    let want = [100, 100, 100, 255];

    // The simulation samples on the CPU and shades with the shader's own
    // function.
    let mut sim = SimGpu::new();
    sim.emulate("heights", |run| {
        let push = bytes_f32(run.push());
        let height = run.sample(0, [push[0], push[1]])[0];
        let color = shaders::shade(height)
            .to_array()
            .map(|c| (c * 255.0).round() as u8);
        run.target().0.copy_from_slice(&color);
    });
    assert_eq!(height_at(&mut sim, &[], uv).unwrap(), want);

    let Some(mut gpu) = gpu() else { return };
    let got = height_at(&mut gpu, &spirv(), uv).unwrap();
    assert!(
        got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 1),
        "{got:?}"
    );
    clean(&gpu);
    // The corner maths is plain Rust too.
    assert_eq!(shaders::corner(2), vec2(0.0, 2.0));
}
