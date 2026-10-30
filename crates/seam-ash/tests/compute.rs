//! Specialization constants, arrays of bindings, indirect dispatches and
//! draws, and what a device reports of itself: on Vulkan under the
//! validation layer and in the simulation, with mistakes failing the same
//! way on both.
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

/// `spirv` with its 32-bit constant `value` made specialization constant
/// `id` (naga writes WGSL's `override`s as plain constants).
fn specialize(mut spirv: Vec<u32>, value: u32, id: u32) -> Vec<u32> {
    const CONSTANT: u32 = 43;
    const SPEC_CONSTANT: u32 = 50;
    const DECORATE: u32 = 71;
    const SPEC_ID: u32 = 1;
    let (mut at, mut result, mut types) = (5, None, None);
    while at < spirv.len() {
        let (len, op) = ((spirv[at] >> 16) as usize, spirv[at] & 0xffff);
        if types.is_none() && (19..=39).contains(&op) {
            types = Some(at);
        }
        if op == CONSTANT && len == 4 && spirv[at + 3] == value {
            spirv[at] = (4 << 16) | SPEC_CONSTANT;
            result = Some(spirv[at + 2]);
        }
        at += len;
    }
    let (Some(result), Some(types)) = (result, types) else {
        panic!("no constant {value}")
    };
    spirv.splice(types..types, [(4 << 16) | DECORATE, result, SPEC_ID, id]);
    spirv
}

fn numbers<G>(
    gpu: &mut G,
    program: Program,
    input: &[u32],
    groups: Option<[u32; 3]>,
) -> Result<Vec<u32>, GpuError>
where
    G: Provide<CreateBuffer>
        + for<'a> Perform<Upload<'a>>
        + for<'a> Perform<Dispatch<'a>>
        + for<'a> Perform<DispatchIndirect<'a>>
        + for<'a> Perform<Download<'a>>,
{
    let bytes = u32_bytes(input);
    gpu.acquire(
        CreateBuffer::new(bytes.len() as u64, BufferUsage::STORAGE),
        |gpu, buf| {
            gpu.perform(Upload::new(*buf, &bytes))?;
            let binds = [Bind::Storage(*buf)];
            match groups {
                Some(groups) => gpu.perform(Dispatch::new(program, &binds, groups))?,
                None => gpu.acquire(CreateBuffer::new(20, BufferUsage::INDIRECT), |gpu, ind| {
                    gpu.perform(Upload::new(
                        *ind,
                        &u32_bytes(&[9, 9, input.len() as u32, 1, 1]),
                    ))?;
                    gpu.perform(DispatchIndirect::new(program, &binds, *ind, 8))
                })?,
            }
            let mut out = vec![0; bytes.len()];
            gpu.perform(Download::new(*buf, &mut out))?;
            Ok(bytes_u32(&out))
        },
    )
}

const SCALE: &str = "
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    data[id.x] = data[id.x] * 777777u;
}";

fn scaled<G>(
    gpu: &mut G,
    spirv: &[u32],
    by: Option<u32>,
    indirect: bool,
) -> Result<Vec<u32>, GpuError>
where
    G: Provide<CreateBuffer>
        + for<'a> Provide<CreateProgram<'a>>
        + for<'a> Perform<Upload<'a>>
        + for<'a> Perform<Dispatch<'a>>
        + for<'a> Perform<DispatchIndirect<'a>>
        + for<'a> Perform<Download<'a>>,
{
    let constants = by.map(|by| [Constant::u32(7, by)]);
    let program = CreateProgram::compute("scale", spirv, "main", &[BindingKind::StorageBuffer])
        .constants(constants.as_ref().map_or(&[], |c| c));
    gpu.acquire(program, |gpu, p| {
        numbers(gpu, *p, &[1, 2, 3, 4], (!indirect).then_some([4, 1, 1]))
    })
}

fn scaling_sim() -> SimGpu {
    let mut sim = SimGpu::new();
    sim.emulate("scale", |run| {
        let by = run.constant(7).unwrap_or(777777);
        let [n, ..] = run.groups();
        let doubled: Vec<u32> = bytes_u32(run.buffer(0))
            .iter()
            .take(n as usize)
            .map(|v| v * by)
            .collect();
        run.buffer(0)[..doubled.len() * 4].copy_from_slice(&u32_bytes(&doubled));
    });
    sim
}

#[test]
fn constants_specialize_every_stage() {
    let spirv = specialize(wgsl(SCALE), 777777, 7);
    let mut sim = scaling_sim();
    assert_eq!(
        scaled(&mut sim, &spirv, Some(3), false).unwrap(),
        [3, 6, 9, 12]
    );
    assert_eq!(
        scaled(&mut sim, &spirv, None, false).unwrap(),
        [777777, 2 * 777777, 3 * 777777, 4 * 777777]
    );
    let Some(mut gpu) = gpu() else { return };
    assert_eq!(
        scaled(&mut gpu, &spirv, Some(3), false).unwrap(),
        [3, 6, 9, 12]
    );
    assert_eq!(
        scaled(&mut gpu, &spirv, None, false).unwrap(),
        [777777, 2 * 777777, 3 * 777777, 4 * 777777]
    );
    clean(&gpu);
}

#[test]
fn dispatches_take_their_counts_from_a_buffer() {
    let spirv = specialize(wgsl(SCALE), 777777, 7);
    assert_eq!(
        scaled(&mut scaling_sim(), &spirv, Some(5), true).unwrap(),
        [5, 10, 15, 20]
    );
    let Some(mut gpu) = gpu() else { return };
    assert_eq!(
        scaled(&mut gpu, &spirv, Some(5), true).unwrap(),
        [5, 10, 15, 20]
    );
    clean(&gpu);
}

const TEXTURES: &str = "
enable wgpu_binding_array;
@group(0) @binding(0) var ts: binding_array<texture_2d<f32>, 3>;
@group(0) @binding(1) var<storage, read_write> out: array<vec4<f32>>;
@compute @workgroup_size(1)
fn main() {
    out[0] = textureLoad(ts[0], vec2<u32>(0u, 0u), 0);
    out[1] = textureLoad(ts[1], vec2<u32>(0u, 0u), 0);
    out[2] = textureLoad(ts[2], vec2<u32>(0u, 0u), 0);
}";

fn arrays<G>(gpu: &mut G, spirv: &[u32]) -> Result<Vec<f32>, GpuError>
where
    G: Provide<CreateImage>
        + Provide<CreateBuffer>
        + for<'a> Provide<CreateProgram<'a>>
        + for<'a> Perform<UploadImage<'a>>
        + for<'a> Perform<Dispatch<'a>>
        + for<'a> Perform<Download<'a>>,
{
    let kinds = [BindingKind::Texture, BindingKind::StorageBuffer];
    let program = CreateProgram::compute("textures", spirv, "main", &kinds).counts(&[3, 1]);
    let tex = CreateImage::new(1, 1, Format::Rgba8Unorm, ImageUsage::SAMPLED);
    gpu.acquire(program, |gpu, p| {
        gpu.acquire(CreateBuffer::new(48, BufferUsage::STORAGE), |gpu, out| {
            gpu.acquire(tex, |gpu, a| {
                gpu.acquire(tex, |gpu, b| {
                    gpu.acquire(tex, |gpu, c| {
                        let (a, b, c) = (*a, *b, *c);
                        for (img, px) in [
                            (a, [255, 0, 0, 255]),
                            (b, [0, 255, 0, 255]),
                            (c, [0, 0, 255, 51]),
                        ] {
                            gpu.perform(UploadImage::new(img, &px))?;
                        }
                        let binds = [
                            Bind::Texture(a),
                            Bind::Texture(b),
                            Bind::Texture(c),
                            Bind::Storage(*out),
                        ];
                        gpu.perform(Dispatch::new(*p, &binds, [1, 1, 1]))?;
                        let mut back = vec![0; 48];
                        gpu.perform(Download::new(*out, &mut back))?;
                        Ok(bytes_f32(&back))
                    })
                })
            })
        })
    })
}

#[test]
fn a_binding_holds_an_array() {
    let spirv = wgsl(TEXTURES);
    let mut sim = SimGpu::new();
    sim.emulate("textures", |run| {
        // Every element is a binding of its own, in order.
        let out: Vec<f32> = (0..3).flat_map(|i| run.texel(i, 0, 0)).collect();
        run.buffer(3).copy_from_slice(&f32_bytes(&out));
    });
    let want = [1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.2];
    assert_eq!(arrays(&mut sim, &spirv).unwrap(), want);
    let Some(mut gpu) = gpu() else { return };
    let real = arrays(&mut gpu, &spirv).unwrap();
    assert!(
        real.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-3),
        "{real:?}"
    );
    clean(&gpu);
}

/// A green triangle covering the target.
const FILL: &str = "
@vertex fn vs(@builtin(vertex_index) i: u32, @builtin(instance_index) n: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }
";

fn indirect_draws<G>(gpu: &mut G, spirv: &[u32]) -> Result<[Vec<u8>; 2], GpuError>
where
    G: Provide<CreateImage>
        + Provide<CreateBuffer>
        + for<'a> Provide<CreateProgram<'a>>
        + for<'a> Perform<Upload<'a>>
        + for<'a> Perform<Draw<'a>>
        + for<'a> Perform<DownloadImage<'a>>,
{
    let pipeline = DrawPipeline::new(spirv, "vs", spirv, "fs", Format::Rgba8Unorm);
    let program = CreateProgram::draw("fill", ProgramKind::Draw(pipeline), &[]);
    let target = CreateImage::new(4, 4, Format::Rgba8Unorm, ImageUsage::TARGET);
    // Two commands: nothing (no instances), then the triangle. Then the
    // same, indexed.
    let plain = u32_bytes(&[3, 0, 0, 0, 3, 1, 0, 0]);
    let indexed = u32_bytes(&[3, 0, 0, 0, 0, 3, 1, 0, 0, 0]);
    gpu.acquire(program, |gpu, p| {
        gpu.acquire(CreateBuffer::new(40, BufferUsage::INDIRECT), |gpu, ind| {
            gpu.acquire(CreateBuffer::new(6, BufferUsage::INDEX), |gpu, idx| {
                gpu.perform(Upload::new(*idx, &[0, 0, 1, 0, 2, 0]))?;
                let mut out = [vec![0; 64], vec![0; 64]];
                for (i, (cmds, index)) in
                    [(&plain, false), (&indexed, true)].into_iter().enumerate()
                {
                    gpu.acquire(target, |gpu, t| {
                        gpu.perform(Upload::new(*ind, cmds))?;
                        let mut d = Draw::new(*p, *t, 0).clear([0.0; 4]);
                        if index {
                            d = d.indexed(*idx, IndexFormat::U16);
                        }
                        gpu.perform(d.indirect_many(*ind, 0, 2, 0))?;
                        gpu.perform(DownloadImage::new(*t, &mut out[i]))
                    })?;
                }
                Ok(out)
            })
        })
    })
}

#[test]
fn draws_take_their_counts_from_a_buffer() {
    let spirv = wgsl(FILL);
    let mut sim = SimGpu::new();
    sim.emulate("fill", |run| {
        let (vertices, instances) = run.counts();
        if vertices == 3 && instances == 1 {
            let (pixels, _, _) = run.target();
            for px in pixels.chunks_mut(4) {
                px.copy_from_slice(&[0, 255, 0, 255]);
            }
        }
    });
    let green: Vec<u8> = [0, 255, 0, 255].repeat(16);
    let want = [green.clone(), green];
    assert_eq!(indirect_draws(&mut sim, &spirv).unwrap(), want);
    let draws = sim
        .log()
        .iter()
        .filter(|r| matches!(r, Recorded::Draw { .. }))
        .count();
    assert_eq!(draws, 4, "each command is a draw");
    let Some(mut gpu) = gpu() else { return };
    if !gpu
        .inspect(DeviceInfo)
        .features
        .contains(GpuFeatures::MULTI_DRAW_INDIRECT)
    {
        eprintln!("skipping: one indirect draw at a time here");
        return;
    }
    assert_eq!(indirect_draws(&mut gpu, &spirv).unwrap(), want);
    clean(&gpu);
}

#[test]
fn devices_report_their_limits() {
    let sim = SimGpu::new().inspect(DeviceInfo);
    assert_eq!(sim.limits, Limits::MINIMUM);
    assert_eq!(sim.features, GpuFeatures::ALL);
    let Some(gpu) = gpu() else { return };
    let info = gpu.inspect(DeviceInfo);
    let (l, min) = (info.limits, Limits::MINIMUM);
    assert!(
        l.workgroup_invocations >= min.workgroup_invocations,
        "{l:?}"
    );
    assert!(l.push_constants >= min.push_constants, "{l:?}");
    assert!(
        l.image_2d >= min.image_2d && l.layers >= min.layers,
        "{l:?}"
    );
    assert!(
        l.subgroup_size >= 1 && l.subgroup_ops.contains(SubgroupOps::BASIC),
        "{l:?}"
    );
    assert!(l.samples >= 4, "{l:?}");
}

fn mistakes<G>(gpu: &mut G, compute: &[u32], draw: &[u32]) -> Vec<String>
where
    G: Provide<CreateImage>
        + Provide<CreateBuffer>
        + for<'a> Provide<CreateProgram<'a>>
        + for<'a> Perform<Upload<'a>>
        + for<'a> Perform<Dispatch<'a>>
        + for<'a> Perform<DispatchIndirect<'a>>
        + for<'a> Perform<Draw<'a>>,
{
    let mut errors = Vec::new();
    let storage = [BindingKind::StorageBuffer];
    let base = CreateProgram::compute("scale", compute, "main", &storage);
    let twice = [Constant::u32(1, 2), Constant::f32(1, 2.0)];
    for bad in [
        base.counts(&[1, 1]),
        base.counts(&[0]),
        base.constants(&twice),
    ] {
        errors.push(
            gpu.acquire(bad, |_, _| Ok::<_, GpuError>(()))
                .unwrap_err()
                .to_string(),
        );
    }
    let pipeline = DrawPipeline::new(draw, "vs", draw, "fs", Format::Rgba8Unorm);
    let drawing = CreateProgram::draw("fill", ProgramKind::Draw(pipeline), &[]);
    let found: Result<Vec<String>, GpuError> = gpu.acquire(base.counts(&[2]), |gpu, p| {
        gpu.acquire(drawing, |gpu, d| {
            gpu.acquire(CreateBuffer::new(32, BufferUsage::STORAGE), |gpu, plain| {
                gpu.acquire(CreateBuffer::new(32, BufferUsage::INDIRECT), |gpu, ind| {
                    gpu.acquire(
                        CreateImage::new(4, 4, Format::Rgba8Unorm, ImageUsage::TARGET),
                        |gpu, t| {
                            let (p, d, plain, ind, t) = (*p, *d, *plain, *ind, *t);
                            let two = [Bind::Storage(plain), Bind::Storage(plain)];
                            let mixed = [Bind::Storage(plain), Bind::Uniform(plain)];
                            let results = [
                                gpu.perform(Dispatch::new(p, &two[..1], [1, 1, 1])),
                                gpu.perform(Dispatch::new(p, &mixed, [1, 1, 1])),
                                gpu.perform(DispatchIndirect::new(p, &two, plain, 0)),
                                gpu.perform(DispatchIndirect::new(p, &two, ind, 2)),
                                gpu.perform(DispatchIndirect::new(p, &two, ind, 24)),
                                gpu.perform(DispatchIndirect::new(d, &[], ind, 0)),
                                gpu.perform(Draw::new(d, t, 0).indirect(plain, 0)),
                                gpu.perform(Draw::new(d, t, 0).indirect_many(ind, 0, 2, 8)),
                                gpu.perform(Draw::new(d, t, 0).indirect_many(ind, 0, 2, 18)),
                                gpu.perform(Draw::new(d, t, 0).indirect_many(ind, 8, 2, 16)),
                            ];
                            Ok(results
                                .into_iter()
                                .map(|r| r.unwrap_err().to_string())
                                .collect::<Vec<_>>())
                        },
                    )
                })
            })
        })
    });
    errors.extend(found.unwrap());
    errors
}

#[test]
fn mistakes_fail_the_same_way_on_both() {
    let (compute, draw) = (wgsl(SCALE), wgsl(FILL));
    let sim = mistakes(&mut SimGpu::new(), &compute, &draw);
    assert_eq!(sim.len(), 13, "{sim:#?}");
    assert!(sim[4].contains("binding 0[1]"), "{sim:#?}");
    let Some(mut gpu) = gpu() else { return };
    assert_eq!(mistakes(&mut gpu, &compute, &draw), sim);
    clean(&gpu);
}

/// A draw, between two timestamps, counting its samples.
fn measured<G>(gpu: &mut G, spirv: &[u32]) -> Result<([u64; 2], u64), GpuError>
where
    G: Provide<CreateImage>
        + Provide<CreateQueries>
        + for<'a> Provide<CreateProgram<'a>>
        + Perform<WriteTimestamp>
        + for<'a> Perform<ReadQueries<'a>>
        + for<'a> Perform<Draw<'a>>,
{
    let pipeline = DrawPipeline::new(spirv, "vs", spirv, "fs", Format::Rgba8Unorm);
    let program = CreateProgram::draw("fill", ProgramKind::Draw(pipeline), &[]);
    let target = CreateImage::new(4, 4, Format::Rgba8Unorm, ImageUsage::TARGET);
    gpu.acquire(program, |gpu, p| {
        gpu.acquire(target, |gpu, t| {
            gpu.acquire(CreateQueries::new(QueryKind::Timestamp, 2), |gpu, times| {
                gpu.acquire(
                    CreateQueries::new(QueryKind::Occlusion, 1),
                    |gpu, samples| {
                        let (times, samples) = (*times, *samples);
                        gpu.perform(WriteTimestamp {
                            queries: times,
                            index: 0,
                        })?;
                        gpu.perform(Draw::new(*p, *t, 3).occlusion(samples, 0))?;
                        gpu.perform(WriteTimestamp {
                            queries: times,
                            index: 1,
                        })?;
                        let (mut at, mut passed) = ([0; 2], [0]);
                        gpu.perform(ReadQueries::new(times, &mut at))?;
                        gpu.perform(ReadQueries::new(samples, &mut passed))?;
                        Ok((at, passed[0]))
                    },
                )
            })
        })
    })
}

#[test]
fn queries_time_work_and_count_samples() {
    let spirv = wgsl(FILL);
    let mut sim = SimGpu::new();
    sim.emulate("fill", |run| run.pass_samples(16));
    let ([start, end], passed) = measured(&mut sim, &spirv).unwrap();
    assert!(start < end, "{start} {end}");
    assert_eq!(passed, 16);
    let Some(mut gpu) = gpu() else { return };
    let features = gpu.inspect(DeviceInfo).features;
    if !features.contains(GpuFeatures::TIMESTAMPS) {
        eprintln!("skipping: no timestamps here");
        return;
    }
    let ([start, end], passed) = measured(&mut gpu, &spirv).unwrap();
    assert!(start <= end, "{start} {end}");
    if features.contains(GpuFeatures::PRECISE_OCCLUSION) {
        assert_eq!(passed, 16);
    } else {
        assert!(passed > 0);
    }
    clean(&gpu);
}

fn tripled<G>(gpu: &mut G, spirv: &[u32]) -> Result<Vec<u32>, GpuError>
where
    G: Provide<CreateBuffer>
        + for<'a> Provide<CreateProgram<'a>>
        + Perform<BufferAddress>
        + for<'a> Perform<Upload<'a>>
        + for<'a> Perform<Dispatch<'a>>
        + for<'a> Perform<Download<'a>>,
{
    let program = CreateProgram::compute("triple", spirv, "main", &[]).push_constants(16);
    gpu.acquire(program, |gpu, p| {
        gpu.acquire(CreateBuffer::new(16, BufferUsage::ADDRESS), |gpu, buf| {
            gpu.perform(Upload::new(*buf, &u32_bytes(&[1, 2, 3, 4])))?;
            let address = gpu.perform(BufferAddress(*buf))?;
            let mut push = address.to_le_bytes().to_vec();
            push.extend(u32_bytes(&[4, 0]));
            gpu.perform(Dispatch::new(*p, &[], [4, 1, 1]).push(&push))?;
            let mut out = vec![0; 16];
            gpu.perform(Download::new(*buf, &mut out))?;
            Ok(bytes_u32(&out))
        })
    })
}

#[test]
fn shaders_reach_buffers_through_their_addresses() {
    let spirv = bytes_u32(include_bytes!("shaders/address.spv"));
    let mut sim = SimGpu::new();
    sim.emulate("triple", |run| {
        let push = run.push();
        let address = u64::from_le_bytes(push[..8].try_into().unwrap());
        let count = u32::from_le_bytes(push[8..12].try_into().unwrap()) as usize;
        let memory = run.memory(address);
        let tripled: Vec<u32> = bytes_u32(&memory[..count * 4])
            .iter()
            .map(|n| n * 3)
            .collect();
        memory[..count * 4].copy_from_slice(&u32_bytes(&tripled));
    });
    assert_eq!(tripled(&mut sim, &spirv).unwrap(), [3, 6, 9, 12]);
    let Some(mut gpu) = gpu() else { return };
    if !gpu
        .inspect(DeviceInfo)
        .features
        .contains(GpuFeatures::ADDRESSES)
    {
        eprintln!("skipping: no buffer addresses here");
        return;
    }
    assert_eq!(tripled(&mut gpu, &spirv).unwrap(), [3, 6, 9, 12]);
    clean(&gpu);
}

fn query_mistakes<G>(gpu: &mut G) -> Vec<String>
where
    G: Provide<CreateBuffer>
        + Provide<CreateQueries>
        + Perform<WriteTimestamp>
        + Perform<BufferAddress>
        + for<'a> Perform<ReadQueries<'a>>,
{
    let mut errors = vec![
        gpu.acquire(CreateQueries::new(QueryKind::Occlusion, 0), |_, _| {
            Ok::<_, GpuError>(())
        })
        .unwrap_err()
        .to_string(),
    ];
    let found: Result<Vec<String>, GpuError> =
        gpu.acquire(CreateQueries::new(QueryKind::Occlusion, 2), |gpu, occ| {
            gpu.acquire(CreateBuffer::new(4, BufferUsage::STORAGE), |gpu, buf| {
                let (occ, buf) = (*occ, *buf);
                let results = [
                    gpu.perform(WriteTimestamp {
                        queries: occ,
                        index: 0,
                    }),
                    gpu.perform(ReadQueries::new(occ, &mut [0; 1])),
                    gpu.perform(ReadQueries::new(occ, &mut [0; 3])),
                    gpu.perform(BufferAddress(buf)).map(|_| ()),
                ];
                Ok(results
                    .into_iter()
                    .map(|r| r.unwrap_err().to_string())
                    .collect())
            })
        });
    errors.extend(found.unwrap());
    errors
}

#[test]
fn query_mistakes_fail_the_same_way_on_both() {
    let sim = query_mistakes(&mut SimGpu::new());
    assert_eq!(sim.len(), 5, "{sim:#?}");
    let Some(mut gpu) = gpu() else { return };
    assert_eq!(query_mistakes(&mut gpu), sim);
    clean(&gpu);
}
