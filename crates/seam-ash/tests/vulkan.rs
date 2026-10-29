//! `Ash` on a real Vulkan driver (llvmpipe, in CI), with the validation
//! layer and synchronization validation on: a missing barrier or a wrong
//! layout fails the test, not just a wrong pixel. Tests that find no Vulkan
//! device, or no validation layer, say so and pass.
#![cfg(feature = "ash")]

use seam_ash::*;
use seam_core::future::block_on;
use seam_core::prelude::*;

/// A device with validation, or `None` (and a note) where there is none.
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

/// Fails if the validation layer reported anything.
fn clean(gpu: &Ash) {
    let messages = gpu.validation_messages();
    assert!(
        messages.is_empty(),
        "validation reported:\n{}",
        messages.join("\n\n")
    );
}

/// SPIR-V for a WGSL module.
fn wgsl(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("the WGSL parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("the WGSL validates");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("SPIR-V is written")
}

const DOUBLE: &str = "
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    data[id.x] = data[id.x] * 2u;
}";

/// The core under test: doubles numbers on the GPU. Generic over the
/// handler, so the same function runs on `Ash` and on `SimGpu`.
fn double<G>(gpu: &mut G, shader: &[u32], numbers: &[u32]) -> Result<Vec<u32>, GpuError>
where
    G: Provide<CreateBuffer>
        + for<'a> Provide<CreateProgram<'a>>
        + for<'a> Perform<Upload<'a>>
        + for<'a> Perform<Dispatch<'a>>
        + for<'a> Perform<Download<'a>>,
{
    let bytes = u32_bytes(numbers);
    let program = CreateProgram::compute("double", shader, "main", &[BindingKind::StorageBuffer]);
    gpu.acquire(program, |gpu, program| {
        gpu.acquire(
            CreateBuffer::new(bytes.len() as u64, BufferUsage::STORAGE),
            |gpu, buf| {
                gpu.perform(Upload::new(*buf, &bytes))?;
                gpu.perform(Dispatch::new(
                    *program,
                    &[Bind::Storage(*buf)],
                    [numbers.len() as u32, 1, 1],
                ))?;
                let mut out = vec![0; bytes.len()];
                gpu.perform(Download::new(*buf, &mut out))?;
                Ok(bytes_u32(&out))
            },
        )
    })
}

#[test]
fn compute_on_the_gpu_and_in_the_simulation() {
    let Some(mut gpu) = gpu() else { return };
    let shader = wgsl(DOUBLE);
    let numbers: Vec<u32> = (0..1000).collect();
    let real = double(&mut gpu, &shader, &numbers).unwrap();
    assert_eq!(real, numbers.iter().map(|n| n * 2).collect::<Vec<_>>());
    clean(&gpu);

    let mut sim = SimGpu::new();
    sim.emulate("double", |run| {
        let doubled = u32_bytes(
            &bytes_u32(run.buffer(0))
                .iter()
                .map(|n| n * 2)
                .collect::<Vec<_>>(),
        );
        run.buffer(0).copy_from_slice(&doubled);
    });
    assert_eq!(double(&mut sim, &shader, &numbers).unwrap(), real);
    assert_eq!(sim.live(), 0, "every scope closed");
}

#[test]
fn the_async_form_awaits_the_same_work() {
    let Some(gpu) = gpu() else { return };
    let shader = wgsl(DOUBLE);
    let out = block_on(async {
        let bytes = u32_bytes(&[5, 6, 7]);
        let program =
            CreateProgram::compute("double", &shader, "main", &[BindingKind::StorageBuffer]);
        gpu.clone()
            .acquire_async(program, async |gpu, program| {
                gpu.clone()
                    .acquire_async(
                        CreateBuffer::new(12, BufferUsage::STORAGE),
                        async |gpu, buf| {
                            gpu.perform_send(Upload::new(*buf, &bytes)).await?;
                            gpu.perform_send(Dispatch::new(
                                *program,
                                &[Bind::Storage(*buf)],
                                [3, 1, 1],
                            ))
                            .await?;
                            let mut out = vec![0; 12];
                            gpu.perform_send(Download::new(*buf, &mut out)).await?;
                            gpu.perform_send(Finish).await?;
                            Ok::<_, GpuError>(bytes_u32(&out))
                        },
                    )
                    .await
            })
            .await
    })
    .unwrap();
    assert_eq!(out, [10, 12, 14]);
    clean(&gpu);
}

/// Several dependent steps in one batch: write, copy, read back the copy,
/// overwrite the first (a write after a read), and a push constant.
#[test]
fn dependent_steps_get_their_barriers() {
    let Some(mut gpu) = gpu() else { return };
    let add = wgsl(
        "
struct Push { amount: u32 }
var<immediate> push: Push;
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x < arrayLength(&data) { data[id.x] = data[id.x] + push.amount; }
}",
    );
    let n = 256u32;
    let size = u64::from(n) * 4;
    let program = CreateProgram::compute("add", &add, "main", &[BindingKind::StorageBuffer])
        .push_constants(4);
    let result = gpu.acquire(program, |gpu, program| {
        gpu.acquire(CreateBuffer::new(size, BufferUsage::STORAGE), |gpu, a| {
            gpu.acquire(CreateBuffer::new(size, BufferUsage::STORAGE), |gpu, b| {
                gpu.perform(Upload::new(*a, &u32_bytes(&vec![1; n as usize])))?;
                gpu.perform(
                    Dispatch::new(*program, &[Bind::Storage(*a)], [n / 64, 1, 1])
                        .push(&10u32.to_le_bytes()),
                )?;
                gpu.perform(CopyBuffer {
                    src: *a,
                    src_offset: 0,
                    dst: *b,
                    dst_offset: 0,
                    size,
                })?;
                // Overwrites `a` after the copy read it.
                gpu.perform(Upload::new(*a, &u32_bytes(&vec![0; n as usize])))?;
                gpu.perform(
                    Dispatch::new(*program, &[Bind::Storage(*b)], [n / 64, 1, 1])
                        .push(&100u32.to_le_bytes()),
                )?;
                let (mut out_a, mut out_b) = (vec![0; size as usize], vec![0; size as usize]);
                gpu.perform(Download::new(*a, &mut out_a))?;
                gpu.perform(Download::new(*b, &mut out_b))?;
                Ok::<_, GpuError>((bytes_u32(&out_a), bytes_u32(&out_b)))
            })
        })
    });
    let (a, b) = result.unwrap();
    assert!(a.iter().all(|&x| x == 0));
    assert!(b.iter().all(|&x| x == 111));
    clean(&gpu);
}

const TRIANGLE: &str = "
struct Out { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> }
@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> Out {
    return Out(vec4<f32>(pos, 0.0, 1.0), color);
}
@fragment
fn fs(in: Out) -> @location(0) vec4<f32> { return in.color; }";

#[test]
fn drawing_into_an_image() {
    let Some(mut gpu) = gpu() else { return };
    let spirv = wgsl(TRIANGLE);
    let kind = ProgramKind::Draw {
        vertex: &spirv,
        vertex_entry: "vs",
        fragment: &spirv,
        fragment_entry: "fs",
        attrs: &[
            VertexAttr {
                offset: 0,
                format: AttrFormat::F32x2,
            },
            VertexAttr {
                offset: 8,
                format: AttrFormat::F32x4,
            },
        ],
        stride: 24,
        target: Format::Rgba8Unorm,
        blend: Blend::Replace,
    };
    // A triangle covering the left half: x from -1 to 0.
    #[rustfmt::skip]
    let vertices = f32_bytes(&[
        -1.0, -1.0,  1.0, 0.0, 0.0, 1.0,
         0.0, -1.0,  1.0, 0.0, 0.0, 1.0,
        -1.0,  3.0,  1.0, 0.0, 0.0, 1.0,
         0.0, -1.0,  1.0, 0.0, 0.0, 1.0,
         0.0,  3.0,  1.0, 0.0, 0.0, 1.0,
        -1.0,  3.0,  1.0, 0.0, 0.0, 1.0,
    ]);
    let (w, h) = (8u32, 4u32);
    let pixels = gpu
        .acquire(
            CreateProgram::draw("triangle", kind, &[]),
            |gpu, program| {
                gpu.acquire(
                    CreateBuffer::new(vertices.len() as u64, BufferUsage::VERTEX),
                    |gpu, vbuf| {
                        gpu.acquire(
                            CreateImage::new(w, h, Format::Rgba8Unorm, ImageUsage::TARGET),
                            |gpu, target| {
                                gpu.perform(Upload::new(*vbuf, &vertices))?;
                                gpu.perform(
                                    Draw::new(*program, *target, 6)
                                        .vertices(*vbuf)
                                        .clear([0.0, 0.0, 1.0, 1.0]),
                                )?;
                                let mut out = vec![0; (w * h * 4) as usize];
                                gpu.perform(DownloadImage::new(*target, &mut out))?;
                                Ok::<_, GpuError>(out)
                            },
                        )
                    },
                )
            },
        )
        .unwrap();
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let want = if x < w / 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            assert_eq!(pixels[i..i + 4], want, "pixel ({x}, {y})");
        }
    }
    clean(&gpu);
}

/// A compute program writing a storage image, then a download of it.
#[test]
fn storage_images() {
    let Some(mut gpu) = gpu() else { return };
    let fill = wgsl(
        "
@group(0) @binding(0) var img: texture_storage_2d<rgba8unorm, write>;
@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    textureStore(img, vec2<i32>(id.xy), vec4<f32>(f32(id.x) / 255.0, f32(id.y) / 255.0, 0.0, 1.0));
}",
    );
    let pixels = gpu
        .acquire(
            CreateProgram::compute("fill", &fill, "main", &[BindingKind::StorageImage]),
            |gpu, program| {
                gpu.acquire(
                    CreateImage::new(4, 4, Format::Rgba8Unorm, ImageUsage::STORAGE),
                    |gpu, img| {
                        gpu.perform(Dispatch::new(
                            *program,
                            &[Bind::StorageImage(*img)],
                            [4, 4, 1],
                        ))?;
                        let mut out = vec![0; 64];
                        gpu.perform(DownloadImage::new(*img, &mut out))?;
                        Ok::<_, GpuError>(out)
                    },
                )
            },
        )
        .unwrap();
    assert_eq!(pixels[(2 * 4 + 3) * 4..(2 * 4 + 3) * 4 + 4], [3, 2, 0, 255]);
    clean(&gpu);
}

/// SPIR-V for GLSL, from glslang where it is installed: WGSL cannot say
/// combined image samplers.
fn glsl(stage: &str, src: &str) -> Option<Vec<u32>> {
    let dir = std::env::temp_dir().join(format!("seam-ash-glsl-{}-{stage}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let (input, output) = (dir.join(format!("shader.{stage}")), dir.join("shader.spv"));
    std::fs::write(&input, src).ok()?;
    let status = std::process::Command::new("glslangValidator")
        .args(["-V", "-o"])
        .arg(&output)
        .arg(&input)
        .output()
        .ok()?;
    if !status.status.success() {
        panic!("glslang: {}", String::from_utf8_lossy(&status.stdout));
    }
    let bytes = std::fs::read(&output).ok()?;
    let _ = std::fs::remove_dir_all(&dir);
    Some(bytes_u32(&bytes))
}

/// Uploads a texture, samples it into a target with premultiplied alpha
/// over a clear colour: what a compositor does for each window.
#[test]
fn sampling_an_uploaded_image() {
    let Some(mut gpu) = gpu() else { return };
    let vs = glsl(
        "vert",
        "#version 450
layout(location = 0) out vec2 uv;
void main() {
    uv = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    gl_Position = vec4(uv * 2.0 - 1.0, 0.0, 1.0);
}",
    );
    let fs = glsl(
        "frag",
        "#version 450
layout(set = 0, binding = 0) uniform sampler2D tex;
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 color;
void main() { color = texture(tex, uv); }",
    );
    let (Some(vs), Some(fs)) = (vs, fs) else {
        eprintln!("skipping: glslang is not installed");
        return;
    };
    let kind = ProgramKind::Draw {
        vertex: &vs,
        vertex_entry: "main",
        fragment: &fs,
        fragment_entry: "main",
        attrs: &[],
        stride: 0,
        target: Format::Rgba8Unorm,
        blend: Blend::PremultipliedAlpha,
    };
    // A 2x2 texture, all half-transparent white, premultiplied.
    let texture = [128u8, 128, 128, 128].repeat(4);
    let pixels = gpu
        .acquire(
            CreateProgram::draw("window", kind, &[BindingKind::SampledImage]),
            |gpu, program| {
                gpu.acquire(
                    CreateImage::new(2, 2, Format::Rgba8Unorm, ImageUsage::SAMPLED),
                    |gpu, tex| {
                        gpu.acquire(
                            CreateImage::new(4, 4, Format::Rgba8Unorm, ImageUsage::TARGET),
                            |gpu, target| {
                                gpu.perform(UploadImage::new(*tex, &texture))?;
                                let bindings = [Bind::Sampled(*tex)];
                                let draw = Draw::new(*program, *target, 3)
                                    .clear([0.0, 0.0, 0.0, 1.0])
                                    .bind(&bindings);
                                gpu.perform(draw)?;
                                let mut out = vec![0; 64];
                                gpu.perform(DownloadImage::new(*target, &mut out))?;
                                Ok::<_, GpuError>(out)
                            },
                        )
                    },
                )
            },
        )
        .unwrap();
    // White at half coverage over black: half grey, fully opaque.
    for px in pixels.chunks(4) {
        assert!((px[0] as i32 - 128).abs() <= 1 && px[3] == 255, "{px:?}");
    }
    clean(&gpu);
}

/// Closing a scope while its resource is still in use defers the
/// destruction; validation would report destroying it in use.
#[test]
fn destroying_in_flight_resources_waits_for_them() {
    let Some(mut gpu) = gpu() else { return };
    let shader = wgsl(DOUBLE);
    let program = CreateProgram::compute("double", &shader, "main", &[BindingKind::StorageBuffer]);
    gpu.acquire(program, |gpu, program| {
        for _ in 0..20 {
            gpu.acquire(CreateBuffer::new(4096, BufferUsage::STORAGE), |gpu, buf| {
                gpu.perform(Upload::new(*buf, &[1; 4096]))?;
                gpu.perform(Dispatch::new(
                    *program,
                    &[Bind::Storage(*buf)],
                    [1024, 1, 1],
                ))?;
                // Submitted, not waited for, then the scope closes.
                gpu.perform(Finish)
            })?;
        }
        gpu.perform(Finish)
    })
    .unwrap();
    clean(&gpu);
}

#[test]
fn mistakes_fail_the_same_way_on_both() {
    let Some(mut gpu) = gpu() else { return };
    fn misuse<G>(gpu: &mut G) -> GpuError
    where
        G: Provide<CreateBuffer> + for<'a> Perform<Upload<'a>>,
    {
        gpu.acquire(CreateBuffer::new(16, BufferUsage::STORAGE), |gpu, buf| {
            gpu.perform(Upload::new(*buf, &[0; 32]))
        })
        .unwrap_err()
    }
    let real = misuse(&mut gpu);
    assert_eq!(real, misuse(&mut SimGpu::new()));
    assert!(matches!(real, GpuError::Invalid(_)), "{real}");
    let info = gpu.inspect(DeviceInfo);
    assert!(info.api_version >= (1, 3, 0));
    clean(&gpu);
}
