//! Rendering on Vulkan, under the validation layer: depth and stencil,
//! index and instance buffers, several targets, multisampling, viewports
//! and scissors, topologies, integer targets; and mistakes, which fail the
//! same way in the simulation.
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

/// A full-screen triangle at `depth`, filled with `color`, both pushed.
const FLAT: &str = "
struct P { color: vec4<f32>, depth: f32 }
var<immediate> p: P;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, p.depth, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return p.color; }
";

/// [`FLAT`]'s push constants.
fn flat(color: [f32; 4], depth: f32) -> Vec<u8> {
    let mut v = color.to_vec();
    v.extend([depth, 0.0, 0.0, 0.0]);
    f32_bytes(&v)
}

const SIZE: u32 = 8;

/// What runs with the images or buffers opened for it.
type With<'f, G, T, R> = &'f mut dyn FnMut(&mut G, &[T]) -> Result<R, GpuError>;

fn px(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * SIZE + x) * 4) as usize;
    pixels[at..at + 4].try_into().unwrap()
}

/// Opens `ops`' images, each in a scope, and runs `f` with all of them.
fn images<G, R>(gpu: &mut G, ops: &[CreateImage], f: With<'_, G, Image, R>) -> Result<R, GpuError>
where
    G: Provide<CreateImage>,
{
    fn go<G: Provide<CreateImage>, R>(
        gpu: &mut G,
        ops: &[CreateImage],
        have: &mut Vec<Image>,
        f: With<'_, G, Image, R>,
    ) -> Result<R, GpuError> {
        match ops.split_first() {
            None => f(gpu, have),
            Some((op, rest)) => gpu.acquire(*op, |gpu, img| {
                have.push(*img);
                go(gpu, rest, have, f)
            }),
        }
    }
    go(gpu, ops, &mut Vec::new(), f)
}

/// Opens a buffer holding each of `data`, for its usage, and runs `f`
/// with all of them.
fn buffers<G, R>(
    gpu: &mut G,
    data: &[(&[u8], BufferUsage)],
    f: With<'_, G, Buffer, R>,
) -> Result<R, GpuError>
where
    G: Provide<CreateBuffer> + for<'a> Perform<Upload<'a>>,
{
    fn go<G: Provide<CreateBuffer> + for<'a> Perform<Upload<'a>>, R>(
        gpu: &mut G,
        data: &[(&[u8], BufferUsage)],
        have: &mut Vec<Buffer>,
        f: With<'_, G, Buffer, R>,
    ) -> Result<R, GpuError> {
        match data.split_first() {
            None => f(gpu, have),
            Some(((bytes, usage), rest)) => {
                gpu.acquire(CreateBuffer::new(bytes.len() as u64, *usage), |gpu, b| {
                    gpu.perform(Upload::new(*b, bytes))?;
                    have.push(*b);
                    go(gpu, rest, have, f)
                })
            }
        }
    }
    go(gpu, data, &mut Vec::new(), f)
}

fn target(format: Format) -> CreateImage {
    CreateImage::new(SIZE, SIZE, format, ImageUsage::TARGET)
}

fn download<G: for<'a> Perform<DownloadImage<'a>>>(
    gpu: &mut G,
    image: Image,
    bytes: usize,
) -> Result<Vec<u8>, GpuError> {
    let mut out = vec![0; bytes];
    gpu.perform(DownloadImage::new(image, &mut out))?;
    Ok(out)
}

#[test]
fn nearer_fragments_win_the_depth_test() {
    let Some(mut gpu) = gpu() else { return };
    let spirv = wgsl(FLAT);
    let pipeline = DrawPipeline::new(&spirv, "vs", &spirv, "fs", Format::Rgba8Unorm)
        .depth(DepthState::new(Format::D32Float));
    let program = CreateProgram::draw("flat", ProgramKind::Draw(pipeline), &[]).push_constants(32);
    let (pixels, depths) = gpu
        .acquire(program, |gpu, program| {
            let ops = [target(Format::Rgba8Unorm), target(Format::D32Float)];
            images(gpu, &ops, &mut |gpu, img| {
                let (color, depth) = (img[0], img[1]);
                // Near red first, then far green: green fails the test.
                let near = flat([1.0, 0.0, 0.0, 1.0], 0.25);
                gpu.perform(
                    Draw::new(*program, color, 3)
                        .clear([0.0, 0.0, 0.0, 1.0])
                        .depth(DepthAttachment::new(depth))
                        .push(&near),
                )?;
                let far = flat([0.0, 1.0, 0.0, 1.0], 0.75);
                gpu.perform(
                    Draw::new(*program, color, 3)
                        .depth(DepthAttachment::new(depth).load(DepthLoad::Keep))
                        .push(&far),
                )?;
                Ok((
                    download(gpu, color, 256)?,
                    bytes_f32(&download(gpu, depth, 256)?),
                ))
            })
        })
        .unwrap();
    assert!(
        pixels.chunks(4).all(|p| p == [255, 0, 0, 255]),
        "{pixels:?}"
    );
    assert!(depths.iter().all(|d| (d - 0.25).abs() < 1e-6), "{depths:?}");
    clean(&gpu);
}

#[test]
fn indexed_instanced_draws_from_two_vertex_buffers() {
    let Some(mut gpu) = gpu() else { return };
    let spirv = wgsl(
        "
struct O { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> }
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) dx: f32, @location(2) c: vec4<f32>) -> O {
    return O(vec4<f32>(p.x + dx, p.y, 0.0, 1.0), c);
}
@fragment fn fs(i: O) -> @location(0) vec4<f32> { return i.color; }
",
    );
    let per_vertex = [VertexAttr::new(0, 0, AttrFormat::F32x2)];
    let per_instance = [
        VertexAttr::new(1, 0, AttrFormat::F32),
        VertexAttr::new(2, 4, AttrFormat::F32x4),
    ];
    let pipeline = DrawPipeline::new(&spirv, "vs", &spirv, "fs", Format::Rgba8Unorm)
        .vertex_buffer(0, VertexLayout::new(8, &per_vertex))
        .vertex_buffer(1, VertexLayout::new(20, &per_instance).per_instance());
    // A quad over the left half; the second instance moves it right.
    let quad = f32_bytes(&[-1.0, -1.0, 0.0, -1.0, -1.0, 1.0, 0.0, 1.0]);
    let instances = f32_bytes(&[0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0]);
    let indices: Vec<u8> = [0u16, 1, 2, 2, 1, 3]
        .iter()
        .flat_map(|i| i.to_le_bytes())
        .collect();
    let data = [
        (quad.as_slice(), BufferUsage::VERTEX),
        (instances.as_slice(), BufferUsage::VERTEX),
        (indices.as_slice(), BufferUsage::INDEX),
    ];
    let program = CreateProgram::draw("quads", ProgramKind::Draw(pipeline), &[]);
    let pixels = gpu
        .acquire(program, |gpu, program| {
            buffers(gpu, &data, &mut |gpu, b| {
                images(gpu, &[target(Format::Rgba8Unorm)], &mut |gpu, img| {
                    gpu.perform(
                        Draw::new(*program, img[0], 6)
                            .clear([0.0; 4])
                            .vertex_buffer(0, b[0], 0)
                            .vertex_buffer(1, b[1], 0)
                            .indexed(b[2], IndexFormat::U16)
                            .instances(2),
                    )?;
                    download(gpu, img[0], 256)
                })
            })
        })
        .unwrap();
    assert_eq!(px(&pixels, 1, 4), [255, 0, 0, 255]);
    assert_eq!(px(&pixels, 6, 4), [0, 0, 255, 255]);
    clean(&gpu);
}

#[test]
fn several_targets_each_blended_its_own_way() {
    let Some(mut gpu) = gpu() else { return };
    let spirv = wgsl(
        "
struct O { @location(0) a: vec4<f32>, @location(1) b: vec4<f32> }
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn fs() -> O { return O(vec4<f32>(1.0, 0.0, 0.0, 1.0), vec4<f32>(0.0, 0.5, 0.0, 0.5)); }
",
    );
    let pipeline = DrawPipeline::new(&spirv, "vs", &spirv, "fs", Format::Rgba8Unorm).target(
        1,
        Some(ColorTarget::new(Format::Rgba8Unorm).blend(Blend::Additive)),
    );
    let program = CreateProgram::draw("two", ProgramKind::Draw(pipeline), &[]);
    let (a, b) = gpu
        .acquire(program, |gpu, program| {
            let ops = [target(Format::Rgba8Unorm), target(Format::Rgba8Unorm)];
            images(gpu, &ops, &mut |gpu, img| {
                gpu.perform(Draw::new(*program, img[0], 3).clear([0.0; 4]).target(
                    1,
                    Some(Attachment::new(img[1]).clear([0.0, 0.0, 0.25, 0.5])),
                ))?;
                Ok((download(gpu, img[0], 256)?, download(gpu, img[1], 256)?))
            })
        })
        .unwrap();
    assert_eq!(px(&a, 3, 3), [255, 0, 0, 255]);
    // Added onto the clear colour.
    assert_eq!(px(&b, 3, 3), [0, 128, 64, 255]);
    clean(&gpu);
}

#[test]
fn multisampled_edges_resolve_to_partial_coverage() {
    let Some(mut gpu) = gpu() else { return };
    let spirv = wgsl(
        "
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0));
    return vec4<f32>(p[i], 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(1.0, 1.0, 1.0, 1.0); }
",
    );
    let pipeline = DrawPipeline::new(&spirv, "vs", &spirv, "fs", Format::Rgba8Unorm).samples(4);
    let program = CreateProgram::draw("half", ProgramKind::Draw(pipeline), &[]);
    let pixels = gpu
        .acquire(program, |gpu, program| {
            let ops = [
                target(Format::Rgba8Unorm).samples(4),
                target(Format::Rgba8Unorm),
            ];
            images(gpu, &ops, &mut |gpu, img| {
                let mut targets = [None; MAX_TARGETS];
                targets[0] = Some(Attachment::new(img[0]).clear([0.0; 4]).resolve(img[1]));
                gpu.perform(Draw::with_targets(*program, targets, 3))?;
                download(gpu, img[1], 256)
            })
        })
        .unwrap();
    // WGSL's y points up: the triangle covers the top left. Above the
    // anti-diagonal is covered, below it is not, and on it, half the
    // samples.
    for y in 0..SIZE {
        for x in 0..SIZE {
            let want = match (x + y).cmp(&(SIZE - 1)) {
                core::cmp::Ordering::Less => 255,
                core::cmp::Ordering::Equal => 128,
                core::cmp::Ordering::Greater => 0,
            };
            assert_eq!(px(&pixels, x, y)[0], want, "at {x}, {y}");
        }
    }
    clean(&gpu);
}

#[test]
fn stencil_masks_a_draw_to_a_scissored_rectangle() {
    let Some(mut gpu) = gpu() else { return };
    let spirv = wgsl(FLAT);
    let format = Format::D32FloatS8Uint;
    let mark = DrawPipeline::new(&spirv, "vs", &spirv, "fs", Format::Rgba8Unorm)
        .target(
            0,
            Some(ColorTarget::new(Format::Rgba8Unorm).write(ColorMask::NONE)),
        )
        .depth(
            DepthState::new(format)
                .compare(Compare::Always)
                .write(false)
                .stencil(StencilFace::new(Compare::Always, StencilOp::Replace)),
        );
    let fill = DrawPipeline::new(&spirv, "vs", &spirv, "fs", Format::Rgba8Unorm).depth(
        DepthState::new(format)
            .compare(Compare::Always)
            .write(false)
            .stencil(StencilFace::new(Compare::Equal, StencilOp::Keep)),
    );
    let mark = CreateProgram::draw("mark", ProgramKind::Draw(mark), &[]).push_constants(32);
    let fill = CreateProgram::draw("fill", ProgramKind::Draw(fill), &[]).push_constants(32);
    let pixels = gpu
        .acquire(mark, |gpu, mark| {
            gpu.acquire(fill, |gpu, fill| {
                let ops = [target(Format::Rgba8Unorm), target(format)];
                images(gpu, &ops, &mut |gpu, img| {
                    let (color, depth) = (img[0], img[1]);
                    let green = flat([0.0, 1.0, 0.0, 1.0], 0.5);
                    // Stencil 1 in the 4x4 square from 2, 2 (and no colour).
                    gpu.perform(
                        Draw::new(*mark, color, 3)
                            .clear([0.0, 0.0, 0.0, 1.0])
                            .depth(DepthAttachment::new(depth))
                            .scissor(Rect::new(2, 2, 4, 4))
                            .stencil_reference(1)
                            .push(&green),
                    )?;
                    // Green wherever the stencil is 1.
                    gpu.perform(
                        Draw::new(*fill, color, 3)
                            .depth(DepthAttachment::new(depth).load(DepthLoad::Keep))
                            .stencil_reference(1)
                            .push(&green),
                    )?;
                    download(gpu, color, 256)
                })
            })
        })
        .unwrap();
    for y in 0..SIZE {
        for x in 0..SIZE {
            let inside = (2..6).contains(&x) && (2..6).contains(&y);
            let want = if inside {
                [0, 255, 0, 255]
            } else {
                [0, 0, 0, 255]
            };
            assert_eq!(px(&pixels, x, y), want, "at {x}, {y}");
        }
    }
    clean(&gpu);
}

#[test]
fn viewports_and_lines() {
    let Some(mut gpu) = gpu() else { return };
    let spirv = wgsl(FLAT);
    let lines = wgsl(
        "
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    return vec4<f32>(select(-1.0, 1.0, i == 1u), 0.125, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 0.0, 1.0, 1.0); }
",
    );
    let tri = DrawPipeline::new(&spirv, "vs", &spirv, "fs", Format::Rgba8Unorm);
    let line = DrawPipeline::new(&lines, "vs", &lines, "fs", Format::Rgba8Unorm)
        .topology(Topology::LineList);
    let tri = CreateProgram::draw("tri", ProgramKind::Draw(tri), &[]).push_constants(32);
    let line = CreateProgram::draw("line", ProgramKind::Draw(line), &[]);
    let pixels = gpu
        .acquire(tri, |gpu, tri| {
            gpu.acquire(line, |gpu, line| {
                images(gpu, &[target(Format::Rgba8Unorm)], &mut |gpu, img| {
                    let red = flat([1.0, 0.0, 0.0, 1.0], 0.0);
                    // Full screen, into the left half's viewport.
                    gpu.perform(
                        Draw::new(*tri, img[0], 3)
                            .clear([0.0; 4])
                            .viewport(Viewport::new(0.0, 0.0, 4.0, 8.0))
                            .push(&red),
                    )?;
                    // A line across row 3 (WGSL's y points up), on top.
                    gpu.perform(Draw::new(*line, img[0], 2))?;
                    download(gpu, img[0], 256)
                })
            })
        })
        .unwrap();
    assert_eq!(px(&pixels, 1, 1), [255, 0, 0, 255]);
    assert_eq!(px(&pixels, 6, 1), [0, 0, 0, 0]);
    for x in 0..SIZE {
        assert_eq!(px(&pixels, x, 3), [0, 0, 255, 255], "row 3, x {x}");
    }
    clean(&gpu);
}

#[test]
fn integer_targets_hold_integers() {
    let Some(mut gpu) = gpu() else { return };
    let spirv = wgsl(
        "
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) u32 { return u32(p.x) + 100u; }
",
    );
    let pipeline = DrawPipeline::new(&spirv, "vs", &spirv, "fs", Format::R32Uint);
    let program = CreateProgram::draw("ids", ProgramKind::Draw(pipeline), &[]);
    let ids = gpu
        .acquire(program, |gpu, program| {
            images(gpu, &[target(Format::R32Uint)], &mut |gpu, img| {
                gpu.perform(Draw::new(*program, img[0], 3).clear([7.0, 0.0, 0.0, 0.0]))?;
                Ok(bytes_u32(&download(gpu, img[0], 256)?))
            })
        })
        .unwrap();
    assert_eq!(&ids[..8], &[100, 101, 102, 103, 104, 105, 106, 107]);
    clean(&gpu);
}

/// Mistakes in programs and draws: each fails, with the same error from
/// both handlers.
fn mistakes<G>(gpu: &mut G, spirv: &[u32]) -> Vec<String>
where
    G: Provide<CreateImage>
        + Provide<CreateBuffer>
        + Provide<CreateSampler>
        + for<'a> Provide<CreateProgram<'a>>
        + for<'a> Perform<Upload<'a>>
        + for<'a> Perform<Draw<'a>>,
{
    let mut errors = Vec::new();
    fn program(p: DrawPipeline<'_>) -> CreateProgram<'_> {
        CreateProgram::draw("p", ProgramKind::Draw(p), &[BindingKind::SampledImage])
            .push_constants(32)
    }
    let two_floats = [VertexAttr::new(0, 0, AttrFormat::F32x2)];
    let flat = |format| DrawPipeline::new(spirv, "vs", spirv, "fs", format);
    // Programs wrong in themselves.
    let bad = [
        flat(Format::D32Float),
        flat(Format::Rgba8Unorm).depth(DepthState::new(Format::Rgba8Unorm)),
        flat(Format::Rgba8Unorm).depth(
            DepthState::new(Format::D32Float)
                .stencil(StencilFace::new(Compare::Always, StencilOp::Keep)),
        ),
        flat(Format::Rgba8Unorm).samples(3),
        flat(Format::R32Uint).blend(Blend::Alpha),
        flat(Format::Rgba8Unorm).vertices(4, &two_floats),
    ];
    for p in bad {
        errors.push(
            gpu.acquire(program(p), |_, _| Ok::<_, GpuError>(()))
                .unwrap_err()
                .to_string(),
        );
    }
    // Draws that do not fit their program.
    let deep = flat(Format::Rgba8Unorm).depth(DepthState::new(Format::D32Float));
    let ops = [
        CreateImage::new(
            4,
            4,
            Format::Rgba8Unorm,
            ImageUsage::TARGET | ImageUsage::SAMPLED,
        ),
        CreateImage::new(4, 4, Format::Bgra8Unorm, ImageUsage::TARGET),
        CreateImage::new(4, 4, Format::D32Float, ImageUsage::TARGET),
        CreateImage::new(2, 2, Format::D32Float, ImageUsage::TARGET),
        CreateImage::new(4, 4, Format::Rgba8Unorm, ImageUsage::SAMPLED),
    ];
    let found = gpu.acquire(program(flat(Format::Rgba8Unorm)), |gpu, plain| {
        gpu.acquire(program(deep), |gpu, deep| {
            gpu.acquire(CreateSampler::new(Filter::Nearest), |gpu, smp| {
                buffers(gpu, &[(&[0; 64], BufferUsage::VERTEX)], &mut |gpu, b| {
                    images(gpu, &ops, &mut |gpu, img| {
                        let [color, bgra, depth, small_depth, tex] =
                            [img[0], img[1], img[2], img[3], img[4]];
                        let (plain, deep, smp, verts) = (*plain, *deep, *smp, b[0]);
                        let push = [0u8; 32];
                        let tex_bind = [Bind::Sampled(tex, smp)];
                        let own_bind = [Bind::Sampled(color, smp)];
                        let mut resolving = [None; MAX_TARGETS];
                        resolving[0] = Some(Attachment::new(color).resolve(tex));
                        let draws = [
                            Draw::new(plain, bgra, 3).bind(&tex_bind).push(&push),
                            Draw::new(plain, color, 3)
                                .bind(&tex_bind)
                                .push(&push)
                                .depth(DepthAttachment::new(depth)),
                            Draw::new(deep, color, 3).bind(&tex_bind).push(&push),
                            Draw::new(deep, color, 3)
                                .bind(&tex_bind)
                                .push(&push)
                                .depth(DepthAttachment::new(small_depth)),
                            Draw::new(plain, color, 3).bind(&own_bind).push(&push),
                            Draw::new(plain, color, 3)
                                .bind(&tex_bind)
                                .push(&push)
                                .vertices(verts),
                            Draw::new(plain, color, 3)
                                .bind(&tex_bind)
                                .push(&push)
                                .indexed(verts, IndexFormat::U16),
                            Draw::with_targets(plain, resolving, 3)
                                .bind(&tex_bind)
                                .push(&push),
                        ];
                        Ok(draws
                            .into_iter()
                            .map(|d| gpu.perform(d).unwrap_err().to_string())
                            .collect::<Vec<_>>())
                    })
                })
            })
        })
    });
    errors.extend(found.unwrap());
    errors
}

#[test]
fn mistakes_fail_the_same_way_on_both() {
    let spirv = wgsl(
        "
@group(0) @binding(0) var t: texture_2d<f32>;
var<immediate> c: vec4<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    return vec4<f32>(f32(i), 0.0, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return c; }
",
    );
    let sim = mistakes(&mut SimGpu::new(), &spirv);
    assert_eq!(sim.len(), 14, "{sim:#?}");
    let Some(mut gpu) = gpu() else { return };
    assert_eq!(mistakes(&mut gpu, &spirv), sim);
    clean(&gpu);
}
