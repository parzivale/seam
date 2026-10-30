//! Presenting: frames drawn into a swapchain's images and shown, on
//! Vulkan through a headless surface under the validation layer, and in
//! `SimGpu`'s simulated window; resizing; and mistakes, which fail the
//! same way on both.
#![cfg(feature = "ash")]

use seam_ash::*;
use seam_core::prelude::*;

fn gpu() -> Option<Ash> {
    match Ash::with(&AshOptions {
        validation: true,
        ..AshOptions::default()
    }) {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            eprintln!("skipping: no Vulkan device with validation here ({e})");
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

fn wgsl(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("the WGSL parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("the WGSL validates");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("SPIR-V")
}

const NOTHING: &str = "
@vertex fn vs() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }
";

const FORMAT: Format = Format::Bgra8Unorm;

/// Three frames, each cleared to its own colour and read back before it
/// is shown; then the window resized, and one more.
fn frames<G>(
    gpu: &mut G,
    spirv: &[u32],
    resized: &mut dyn FnMut(&G, Swapchain),
) -> Result<Vec<Vec<u8>>, GpuError>
where
    G: Provide<CreateSwapchain>
        + for<'a> Provide<CreateProgram<'a>>
        + Perform<NextFrame>
        + Perform<Present>
        + Perform<ResizeSwapchain>
        + for<'a> Perform<Draw<'a>>
        + for<'a> Perform<DownloadImage<'a>>,
{
    let pipeline = DrawPipeline::new(spirv, "vs", spirv, "fs", FORMAT);
    let program = CreateProgram::draw("nothing", ProgramKind::Draw(pipeline), &[]);
    gpu.acquire(program, |gpu, p| {
        let p = *p;
        gpu.acquire(
            CreateSwapchain::headless(4, 4).format(FORMAT),
            |gpu, chain| {
                let chain = *chain;
                let mut seen = Vec::new();
                let mut frame = |gpu: &mut G, rgba: [f32; 4], len: usize| -> Result<(), GpuError> {
                    let image = gpu.perform(NextFrame(chain))?;
                    gpu.perform(Draw::new(p, image, 0).clear(rgba))?;
                    let mut out = vec![0; len];
                    gpu.perform(DownloadImage::new(image, &mut out))?;
                    seen.push(out);
                    gpu.perform(Present(chain))
                };
                for rgba in [
                    [1.0, 0.0, 0.0, 1.0],
                    [0.0, 1.0, 0.0, 1.0],
                    [0.0, 0.0, 1.0, 1.0],
                ] {
                    frame(gpu, rgba, 64)?;
                }
                resized(gpu, chain);
                match gpu.perform(NextFrame(chain)) {
                    Err(GpuError::Outdated) => {}
                    Ok(_) => gpu.perform(Present(chain))?,
                    Err(e) => return Err(e),
                }
                gpu.perform(ResizeSwapchain {
                    swapchain: chain,
                    width: 2,
                    height: 2,
                })?;
                frame(gpu, [1.0, 1.0, 1.0, 1.0], 16)?;
                Ok(seen)
            },
        )
    })
}

fn want() -> Vec<Vec<u8>> {
    // Blue, green, red, alpha: BGRA.
    vec![
        [0, 0, 255, 255].repeat(16),
        [0, 255, 0, 255].repeat(16),
        [255, 0, 0, 255].repeat(16),
        [255; 4].repeat(4),
    ]
}

#[test]
fn frames_are_drawn_and_presented() {
    let spirv = wgsl(NOTHING);
    let sim = SimGpu::new();
    let seen = frames(&mut sim.clone(), &spirv, &mut |sim, chain| {
        sim.resize_window(chain, 2, 2);
    })
    .unwrap();
    assert_eq!(seen, want());
    let log = sim.log();
    assert_eq!(
        log.iter()
            .filter(|r| matches!(r, Recorded::Present(_)))
            .count(),
        4
    );
    assert!(
        log.iter()
            .any(|r| matches!(r, Recorded::ResizeSwapchain(_, 2, 2)))
    );
    assert_eq!(sim.live(), 0, "every scope closed");

    let Some(mut gpu) = gpu() else { return };
    match frames(&mut gpu, &spirv, &mut |_, _| {}) {
        Err(GpuError::Unsupported(why)) => eprintln!("skipping: {why}"),
        seen => assert_eq!(seen.unwrap(), want()),
    }
    clean(&gpu);
}

#[test]
fn the_simulated_window_keeps_what_it_was_shown() {
    let sim = SimGpu::new();
    let spirv = wgsl(NOTHING);
    let pipeline = DrawPipeline::new(&spirv, "vs", &spirv, "fs", FORMAT);
    let program = CreateProgram::draw("nothing", ProgramKind::Draw(pipeline), &[]);
    let shown = sim
        .clone()
        .acquire(program, |gpu, p| {
            gpu.acquire(
                CreateSwapchain::headless(1, 1).format(FORMAT),
                |gpu, chain| {
                    for rgba in [[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]] {
                        let image = gpu.perform(NextFrame(*chain))?;
                        gpu.perform(Draw::new(*p, image, 0).clear(rgba))?;
                        gpu.perform(Present(*chain))?;
                    }
                    Ok::<_, GpuError>(sim.frames(*chain))
                },
            )
        })
        .unwrap();
    assert_eq!(shown, [vec![0, 0, 255, 255], vec![255, 0, 0, 255]]);
}

fn mistakes<G>(gpu: &mut G, spirv: &[u32]) -> Vec<String>
where
    G: Provide<CreateSwapchain>
        + for<'a> Provide<CreateProgram<'a>>
        + Perform<NextFrame>
        + Perform<Present>
        + Perform<ResizeSwapchain>
        + for<'a> Perform<Draw<'a>>,
{
    let mut errors = Vec::new();
    for bad in [
        CreateSwapchain::headless(0, 4),
        CreateSwapchain::headless(4, 4).format(Format::D32Float),
    ] {
        errors.push(
            gpu.acquire(bad, |_, _| Ok::<_, GpuError>(()))
                .unwrap_err()
                .to_string(),
        );
    }
    let pipeline = DrawPipeline::new(spirv, "vs", spirv, "fs", FORMAT);
    let program = CreateProgram::draw("nothing", ProgramKind::Draw(pipeline), &[]);
    let found: Result<Vec<String>, GpuError> = gpu.acquire(program, |gpu, p| {
        let p = *p;
        gpu.acquire(
            CreateSwapchain::headless(4, 4).format(FORMAT),
            |gpu, chain| {
                let chain = *chain;
                let mut errors = vec![gpu.perform(Present(chain)).unwrap_err().to_string()];
                let image = gpu.perform(NextFrame(chain))?;
                errors.push(gpu.perform(NextFrame(chain)).unwrap_err().to_string());
                let resize = ResizeSwapchain {
                    swapchain: chain,
                    width: 8,
                    height: 8,
                };
                errors.push(gpu.perform(resize).unwrap_err().to_string());
                gpu.perform(Draw::new(p, image, 0).clear([0.0; 4]))?;
                gpu.perform(Present(chain))?;
                // A frame once shown is gone.
                errors.push(gpu.perform(Draw::new(p, image, 0)).unwrap_err().to_string());
                errors.push(
                    gpu.perform(ResizeSwapchain { width: 0, ..resize })
                        .unwrap_err()
                        .to_string(),
                );
                Ok(errors)
            },
        )
    });
    match found {
        Ok(found) => errors.extend(found),
        Err(GpuError::Unsupported(why)) => errors.push(why),
        Err(e) => panic!("{e}"),
    }
    errors
}

#[test]
fn mistakes_fail_the_same_way_on_both() {
    let spirv = wgsl(NOTHING);
    let sim = mistakes(&mut SimGpu::new(), &spirv);
    assert_eq!(sim.len(), 7, "{sim:#?}");
    let Some(mut gpu) = gpu() else { return };
    let real = mistakes(&mut gpu, &spirv);
    if real.len() == 3 {
        eprintln!("skipping: {}", real[2]);
        return;
    }
    assert_eq!(real, sim);
    clean(&gpu);
}
