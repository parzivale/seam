//! Images beyond one 2D level, on Vulkan under the validation layer and in
//! the simulation: mip levels, arrays, cube maps and volumes; regions of
//! them moved in and out; copies, blits, mip chains and buffer fills; and
//! mistakes, which fail the same way on both.
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

/// What runs with the images opened for it.
type With<'f, G, R> = &'f mut dyn FnMut(&mut G, &[Image]) -> Result<R, GpuError>;

/// Opens `ops`' images, each in a scope, and runs `f` with all of them.
fn images<G, R>(gpu: &mut G, ops: &[CreateImage], f: With<'_, G, R>) -> Result<R, GpuError>
where
    G: Provide<CreateImage>,
{
    fn go<G: Provide<CreateImage>, R>(
        gpu: &mut G,
        ops: &[CreateImage],
        have: &mut Vec<Image>,
        f: With<'_, G, R>,
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

fn read<G: for<'a> Perform<DownloadImage<'a>>>(
    gpu: &mut G,
    image: Image,
    op: &CreateImage,
    region: ImageRegion,
) -> Result<Vec<u8>, GpuError> {
    let mut out = vec![0; region.bytes(op) as usize];
    gpu.perform(DownloadImage::new(image, &mut out).region(region))?;
    Ok(out)
}

/// `n` RGBA pixels, each `[i, 2i, 3i, 255]` from `first`.
fn ramp(first: u8, n: usize) -> Vec<u8> {
    (0..n)
        .flat_map(|i| {
            let v = first.wrapping_add(i as u8);
            [v, v.wrapping_mul(2), v.wrapping_mul(3), 255]
        })
        .collect()
}

/// Everything the transfers read back, in order.
fn transfers<G>(gpu: &mut G) -> Result<Vec<Vec<u8>>, GpuError>
where
    G: Provide<CreateImage>
        + Provide<CreateBuffer>
        + for<'a> Perform<Upload<'a>>
        + for<'a> Perform<Download<'a>>
        + for<'a> Perform<UploadImage<'a>>
        + for<'a> Perform<DownloadImage<'a>>
        + Perform<CopyImage>
        + Perform<BlitImage>
        + Perform<GenerateMips>
        + Perform<FillBuffer>,
{
    let rgba = Format::Rgba8Unorm;
    let array = CreateImage::new(4, 4, rgba, ImageUsage::SAMPLED)
        .layers(2)
        .mips(3);
    let other = CreateImage::new(4, 4, rgba, ImageUsage::SAMPLED).layers(2);
    let small = CreateImage::new(2, 2, rgba, ImageUsage::SAMPLED);
    let chain = CreateImage::new(8, 8, rgba, ImageUsage::SAMPLED).full_mips();
    let cube = CreateImage::cube(2, 1, rgba, ImageUsage::SAMPLED);
    let volume = CreateImage::volume(4, 4, 4, Format::R8Unorm, ImageUsage::SAMPLED);
    let solid = CreateImage::volume(4, 4, 4, rgba, ImageUsage::SAMPLED).full_mips();
    let ops = [array, other, small, chain, cube, volume, solid];
    images(gpu, &ops, &mut |gpu, img| {
        let [
            array_i,
            other_i,
            small_i,
            chain_i,
            cube_i,
            volume_i,
            solid_i,
        ] = img.try_into().unwrap();
        let mut out = Vec::new();

        // Both layers of level 0, then a rectangle of layer 1, then all
        // of level 1.
        gpu.perform(UploadImage::new(array_i, &ramp(0, 32)))?;
        gpu.perform(
            UploadImage::new(array_i, &ramp(200, 4))
                .region(ImageRegion::ALL.layers(1, 1).rect(1, 1, 2, 2)),
        )?;
        gpu.perform(UploadImage::new(array_i, &ramp(100, 8)).region(ImageRegion::level(1)))?;
        out.push(read(gpu, array_i, &array, ImageRegion::ALL)?);
        out.push(read(
            gpu,
            array_i,
            &array,
            ImageRegion::level(1).layers(1, 1),
        )?);
        out.push(read(
            gpu,
            array_i,
            &array,
            ImageRegion::ALL.layers(1, 1).rect(0, 1, 4, 2),
        )?);

        // A copy of both layers' middle into the other's corner.
        gpu.perform(UploadImage::new(other_i, &[0; 128]))?;
        gpu.perform(CopyImage {
            src: array_i,
            src_at: ImageAt::new(0, 0, [1, 1, 0]),
            dst: other_i,
            dst_at: ImageAt::new(0, 0, [2, 2, 0]),
            extent: [2, 2, 1],
            layers: 2,
        })?;
        out.push(read(gpu, other_i, &other, ImageRegion::ALL)?);

        // Layer 0 of level 0, blitted down to half.
        gpu.perform(BlitImage {
            src: array_i,
            src_mip: 0,
            src_layer: 0,
            src_rect: [0, 0, 4, 4],
            dst: small_i,
            dst_mip: 0,
            dst_layer: 0,
            dst_rect: [0, 0, 2, 2],
            filter: Filter::Linear,
        })?;
        out.push(read(gpu, small_i, &small, ImageRegion::ALL)?);

        // A mip chain from a checkerboard: every level below the first is
        // grey.
        let board: Vec<u8> = (0..64)
            .flat_map(|i| {
                let v = if (i % 8 + i / 8) % 2 == 0 { 255 } else { 0 };
                [v, v, v, 255]
            })
            .collect();
        gpu.perform(UploadImage::new(chain_i, &board))?;
        gpu.perform(GenerateMips { image: chain_i })?;
        for mip in 1..chain.mips {
            out.push(read(gpu, chain_i, &chain, ImageRegion::level(mip))?);
        }

        // A cube's faces one at a time, read back whole.
        for face in 0..6 {
            gpu.perform(
                UploadImage::new(cube_i, &ramp(face as u8 * 10, 4))
                    .region(ImageRegion::ALL.layers(face, 1)),
            )?;
        }
        out.push(read(gpu, cube_i, &cube, ImageRegion::ALL)?);

        // A volume, then a box inside it, read back whole and boxed.
        let slices: Vec<u8> = (0..64).collect();
        gpu.perform(UploadImage::new(volume_i, &slices))?;
        gpu.perform(
            UploadImage::new(volume_i, &[200; 8])
                .region(ImageRegion::ALL.boxed([1, 1, 1], [2, 2, 2])),
        )?;
        out.push(read(gpu, volume_i, &volume, ImageRegion::ALL)?);
        out.push(read(
            gpu,
            volume_i,
            &volume,
            ImageRegion::ALL.boxed([0, 1, 2], [4, 2, 2]),
        )?);

        // A volume's mip chain halves in depth too.
        let halves: Vec<u8> = (0..64)
            .flat_map(|i| {
                let v = if i < 32 { 0 } else { 200 };
                [v, v, v, 255]
            })
            .collect();
        gpu.perform(UploadImage::new(solid_i, &halves))?;
        gpu.perform(GenerateMips { image: solid_i })?;
        out.push(read(gpu, solid_i, &solid, ImageRegion::level(2))?);

        // A buffer filled in the middle.
        out.push(
            gpu.acquire(CreateBuffer::new(16, BufferUsage::STORAGE), |gpu, buf| {
                gpu.perform(Upload::new(*buf, &[1; 16]))?;
                gpu.perform(FillBuffer {
                    buffer: *buf,
                    offset: 4,
                    size: 8,
                    value: 0xdead_beef,
                })?;
                let mut back = vec![0; 16];
                gpu.perform(Download::new(*buf, &mut back))?;
                Ok(back)
            })?,
        );
        Ok(out)
    })
}

/// Within `tolerance` of each other, byte by byte.
fn near(a: &[u8], b: &[u8], tolerance: u8) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= tolerance)
}

#[test]
fn transfers_move_levels_layers_faces_and_slices() {
    let sim = transfers(&mut SimGpu::new()).unwrap();
    let [
        array,
        array_level_1,
        array_rows,
        copied,
        blitted,
        chain_1,
        chain_2,
        chain_3,
        cube,
        volume,
        volume_box,
        solid_2,
        filled,
    ] = &sim[..]
    else {
        panic!("{} reads", sim.len())
    };

    // Layer 1's middle was overwritten; layer 0 was not.
    let px = |data: &[u8], layer: usize, x: usize, y: usize| {
        let at = ((layer * 16) + y * 4 + x) * 4;
        data[at..at + 4].to_vec()
    };
    assert_eq!(px(array, 0, 1, 1), ramp(5, 1));
    assert_eq!(px(array, 1, 1, 1), ramp(200, 1));
    assert_eq!(px(array, 1, 2, 2), ramp(203, 1));
    assert_eq!(px(array, 1, 0, 0), ramp(16, 1));
    assert_eq!(array_level_1, &ramp(104, 4));
    assert_eq!(&array_rows[..], &array[16 * 4 + 16..16 * 4 + 48]);

    // Each layer's middle, in the other's corner.
    assert_eq!(px(copied, 0, 0, 0), [0; 4]);
    assert_eq!(px(copied, 0, 2, 2), ramp(5, 1));
    assert_eq!(px(copied, 0, 3, 3), ramp(10, 1));
    assert_eq!(px(copied, 1, 2, 2), ramp(200, 1));

    // Each 2x2 block of layer 0, averaged.
    let average = |x: usize, y: usize| {
        let at = |dx, dy| px(array, 0, x * 2 + dx, y * 2 + dy);
        (0..4)
            .map(|k| {
                let sum: u32 = [at(0, 0), at(1, 0), at(0, 1), at(1, 1)]
                    .iter()
                    .map(|p| u32::from(p[k]))
                    .sum();
                (sum as f32 / 4.0).round() as u8
            })
            .collect::<Vec<_>>()
    };
    for (i, (x, y)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
        assert!(near(&blitted[i * 4..i * 4 + 4], &average(x, y), 1));
    }

    // A checkerboard halves to grey, all the way down.
    for level in [chain_1, chain_2, chain_3] {
        assert!(level.chunks(4).all(|p| near(p, &[128, 128, 128, 255], 1)));
    }

    let faces: Vec<u8> = (0..6).flat_map(|f| ramp(f * 10, 4)).collect();
    assert_eq!(cube, &faces);

    // The box is overwritten, the rest is the ramp.
    for (i, v) in volume.iter().enumerate() {
        let (x, y, z) = (i % 4, i / 4 % 4, i / 16);
        let inside = (1..3).contains(&x) && (1..3).contains(&y) && (1..3).contains(&z);
        assert_eq!(
            *v,
            if inside { 200 } else { i as u8 },
            "texel {x}, {y}, {z}"
        );
    }
    let boxed: Vec<u8> = (2..4)
        .flat_map(|z| (1..3).flat_map(move |y| (0..4).map(move |x| (z, y, x))))
        .map(|(z, y, x)| volume[z * 16 + y * 4 + x])
        .collect();
    assert_eq!(volume_box, &boxed);

    // Level 2 of a 4x4x4 is 1x1x1: both halves, averaged.
    assert!(near(solid_2, &[100, 100, 100, 255], 1));

    assert_eq!(&filled[..4], &[1; 4]);
    assert_eq!(
        &filled[4..12],
        &[0xef, 0xbe, 0xad, 0xde, 0xef, 0xbe, 0xad, 0xde]
    );
    assert_eq!(&filled[12..], &[1; 4]);

    let Some(mut gpu) = gpu() else { return };
    let real = transfers(&mut gpu).unwrap();
    assert_eq!(real.len(), sim.len());
    for (i, (real, sim)) in real.iter().zip(&sim).enumerate() {
        assert!(near(real, sim, 1), "read {i}:\n{real:?}\n{sim:?}");
    }
    clean(&gpu);
}

/// Reads a cube's faces, an array's layers and a volume's texel into a
/// buffer.
const LOOKS: &str = "
@group(0) @binding(0) var cube: texture_cube<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var layers: texture_2d_array<f32>;
@group(0) @binding(3) var volume: texture_3d<f32>;
@group(0) @binding(4) var<storage, read_write> out: array<vec4<f32>>;
@compute @workgroup_size(1)
fn main() {
    let dirs = array<vec3<f32>, 6>(
        vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(-1.0, 0.0, 0.0),
        vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, -1.0, 0.0),
        vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, -1.0),
    );
    for (var i = 0u; i < 6u; i++) {
        out[i] = textureSampleLevel(cube, smp, dirs[i], 0.0);
    }
    for (var l = 0u; l < 3u; l++) {
        out[6u + l] = textureLoad(layers, vec2<u32>(1u, 0u), l, 0);
    }
    out[9] = textureLoad(volume, vec3<u32>(1u, 2u, 3u), 0);
}";

fn looks<G>(gpu: &mut G, spirv: &[u32]) -> Result<Vec<f32>, GpuError>
where
    G: Provide<CreateImage>
        + Provide<CreateBuffer>
        + Provide<CreateSampler>
        + for<'a> Provide<CreateProgram<'a>>
        + for<'a> Perform<UploadImage<'a>>
        + for<'a> Perform<Dispatch<'a>>
        + for<'a> Perform<Download<'a>>,
{
    use BindingKind as K;
    let kinds = [
        K::Texture,
        K::Sampler,
        K::Texture,
        K::Texture,
        K::StorageBuffer,
    ];
    let ops = [
        CreateImage::cube(1, 1, Format::Rgba8Unorm, ImageUsage::SAMPLED),
        CreateImage::new(2, 2, Format::Rgba8Unorm, ImageUsage::SAMPLED).layers(3),
        CreateImage::volume(2, 4, 4, Format::R8Unorm, ImageUsage::SAMPLED),
    ];
    gpu.acquire(
        CreateProgram::compute("looks", spirv, "main", &kinds),
        |gpu, p| {
            gpu.acquire(CreateSampler::new(Filter::Nearest), |gpu, smp| {
                gpu.acquire(CreateBuffer::new(160, BufferUsage::STORAGE), |gpu, buf| {
                    let (p, smp, buf) = (*p, *smp, *buf);
                    images(gpu, &ops, &mut |gpu, img| {
                        let faces: Vec<u8> = (0..6)
                            .flat_map(|f| [f * 40, 255 - f * 40, 7, 255])
                            .collect();
                        gpu.perform(UploadImage::new(img[0], &faces))?;
                        gpu.perform(UploadImage::new(img[1], &ramp(0, 12)))?;
                        let slices: Vec<u8> = (0..32).map(|i| i * 8).collect();
                        gpu.perform(UploadImage::new(img[2], &slices))?;
                        let binds = [
                            Bind::Texture(img[0]),
                            Bind::Sampler(smp),
                            Bind::Texture(img[1]),
                            Bind::Texture(img[2]),
                            Bind::Storage(buf),
                        ];
                        gpu.perform(Dispatch::new(p, &binds, [1, 1, 1]))?;
                        let mut out = vec![0; 160];
                        gpu.perform(Download::new(buf, &mut out))?;
                        Ok(bytes_f32(&out))
                    })
                })
            })
        },
    )
}

#[test]
fn programs_read_cubes_arrays_and_volumes() {
    let spirv = wgsl(LOOKS);
    let mut sim = SimGpu::new();
    sim.emulate("looks", |run| {
        let mut out = Vec::new();
        for face in 0..6 {
            out.extend(run.texel_in(0, 0, face, 0, 0));
        }
        for layer in 0..3 {
            out.extend(run.texel_in(2, 0, layer, 1, 0));
        }
        out.extend(run.texel_in(3, 0, 3, 1, 2));
        run.buffer(4).copy_from_slice(&f32_bytes(&out));
    });
    let want = looks(&mut sim, &spirv).unwrap();
    let unorm = |v: u8| f32::from(v) / 255.0;
    assert_eq!(want[4 * 2..4 * 3], [unorm(80), unorm(175), unorm(7), 1.0]);
    assert_eq!(
        want[4 * 7..4 * 8],
        ramp(5, 1).into_iter().map(unorm).collect::<Vec<_>>()
    );
    assert_eq!(want[4 * 9], unorm((3 * 8 + 2 * 2 + 1) * 8));

    let Some(mut gpu) = gpu() else { return };
    let real = looks(&mut gpu, &spirv).unwrap();
    assert!(
        real.iter().zip(&want).all(|(a, b)| (a - b).abs() < 1e-3),
        "{real:?}\n{want:?}"
    );
    clean(&gpu);
}

#[test]
fn compressed_images_move_in_whole_blocks() {
    fn go<G>(gpu: &mut G) -> Result<Vec<u8>, GpuError>
    where
        G: Provide<CreateImage>
            + for<'a> Perform<UploadImage<'a>>
            + for<'a> Perform<DownloadImage<'a>>,
    {
        let op = CreateImage::new(8, 8, Format::Bc1Unorm, ImageUsage::SAMPLED);
        gpu.acquire(op, |gpu, img| {
            let blocks: Vec<u8> = (0..32).collect();
            gpu.perform(UploadImage::new(*img, &blocks))?;
            gpu.perform(UploadImage::new(*img, &[9; 8]).region(ImageRegion::ALL.rect(4, 4, 4, 4)))?;
            read(gpu, *img, &op, ImageRegion::ALL)
        })
    }
    let sim = go(&mut SimGpu::new()).unwrap();
    assert_eq!(&sim[..24], &(0..24).collect::<Vec<u8>>()[..]);
    assert_eq!(&sim[24..], &[9; 8]);
    let Some(mut gpu) = gpu() else { return };
    match go(&mut gpu) {
        Err(GpuError::Unsupported(why)) => eprintln!("skipping: {why}"),
        real => assert_eq!(real.unwrap(), sim),
    }
    clean(&gpu);
}

fn mistakes<G>(gpu: &mut G) -> Vec<String>
where
    G: Provide<CreateImage>
        + Provide<CreateBuffer>
        + for<'a> Perform<UploadImage<'a>>
        + for<'a> Perform<DownloadImage<'a>>
        + Perform<CopyImage>
        + Perform<BlitImage>
        + Perform<GenerateMips>
        + Perform<FillBuffer>,
{
    let mut errors = Vec::new();
    // Images wrong in themselves.
    let bad = [
        CreateImage::new(4, 4, Format::Rgba8Unorm, ImageUsage::SAMPLED).mips(4),
        CreateImage::new(4, 2, Format::Rgba8Unorm, ImageUsage::SAMPLED)
            .layers(6)
            .mips(1),
        CreateImage::volume(4, 4, 4, Format::Rgba8Unorm, ImageUsage::SAMPLED).layers(2),
        CreateImage::new(8, 8, Format::Bc1Unorm, ImageUsage::TARGET),
        CreateImage::new(4, 4, Format::Rgba8Unorm, ImageUsage::SAMPLED).layers(0),
    ];
    let mut bad = bad.to_vec();
    bad[1].kind = ImageKind::Cube;
    for op in bad {
        errors.push(
            gpu.acquire(op, |_, _| Ok::<_, GpuError>(()))
                .unwrap_err()
                .to_string(),
        );
    }
    let rgba = |w, h| CreateImage::new(w, h, Format::Rgba8Unorm, ImageUsage::SAMPLED);
    let ops = [
        rgba(4, 4).mips(2),
        rgba(4, 4),
        CreateImage::new(4, 4, Format::R32Uint, ImageUsage::SAMPLED).mips(2),
        CreateImage::new(4, 4, Format::D32FloatS8Uint, ImageUsage::TARGET),
        CreateImage::new(4, 4, Format::Rgba8Unorm, ImageUsage::TARGET).samples(4),
        CreateImage::new(8, 8, Format::Bc1Unorm, ImageUsage::SAMPLED),
        CreateImage::new(4, 4, Format::R16Float, ImageUsage::SAMPLED),
    ];
    let found = gpu.acquire(CreateBuffer::new(16, BufferUsage::STORAGE), |gpu, buf| {
        let buf = *buf;
        images(gpu, &ops, &mut |gpu, img| {
            let [mipped, plain, ints, stencil, multi, bc, half] = img.try_into().unwrap();
            let blit = |src, dst, filter| BlitImage {
                src,
                src_mip: 0,
                src_layer: 0,
                src_rect: [0, 0, 4, 4],
                dst,
                dst_mip: 0,
                dst_layer: 0,
                dst_rect: [0, 0, 2, 2],
                filter,
            };
            let mut out = [0; 64];
            let results = [
                gpu.perform(UploadImage::new(plain, &[0; 60])),
                gpu.perform(UploadImage::new(plain, &[0; 4]).region(ImageRegion::level(1))),
                gpu.perform(
                    UploadImage::new(plain, &[0; 16]).region(ImageRegion::ALL.rect(3, 3, 2, 2)),
                ),
                gpu.perform(
                    UploadImage::new(plain, &[0; 64]).region(ImageRegion::ALL.layers(1, 1)),
                ),
                gpu.perform(
                    UploadImage::new(bc, &[0; 8]).region(ImageRegion::ALL.rect(2, 0, 4, 4)),
                ),
                gpu.perform(DownloadImage::new(stencil, &mut [0; 80])),
                gpu.perform(DownloadImage::new(multi, &mut out)),
                gpu.perform(CopyImage::whole(plain, plain, 4, 4)),
                gpu.perform(CopyImage::whole(plain, half, 4, 4)),
                gpu.perform(CopyImage::whole(mipped, plain, 5, 4)),
                gpu.perform(blit(ints, mipped, Filter::Linear)),
                gpu.perform(blit(plain, ints, Filter::Nearest)),
                gpu.perform(blit(bc, plain, Filter::Nearest)),
                gpu.perform(GenerateMips { image: plain }),
                gpu.perform(GenerateMips { image: ints }),
                gpu.perform(FillBuffer {
                    buffer: buf,
                    offset: 2,
                    size: 4,
                    value: 0,
                }),
                gpu.perform(FillBuffer {
                    buffer: buf,
                    offset: 8,
                    size: 12,
                    value: 0,
                }),
            ];
            Ok(results
                .into_iter()
                .enumerate()
                .map(|(i, r)| {
                    r.map(|()| panic!("mistake {i} worked"))
                        .unwrap_err()
                        .to_string()
                })
                .collect::<Vec<_>>())
        })
    });
    errors.extend(found.unwrap());
    errors
}

#[test]
fn mistakes_fail_the_same_way_on_both() {
    let sim = mistakes(&mut SimGpu::new());
    assert_eq!(sim.len(), 22, "{sim:#?}");
    let Some(mut gpu) = gpu() else { return };
    assert_eq!(mistakes(&mut gpu), sim);
    clean(&gpu);
}
