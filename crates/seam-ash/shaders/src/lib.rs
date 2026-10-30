//! Shaders written in Rust, compiled to SPIR-V by rust-gpu
//! (`crates/seam-ash/shaders/build`, which writes `shaders.spv` next to
//! this crate), for `seam-ash`'s tests and `seam`'s examples.
//!
//! The same crate builds for the CPU on stable Rust, so a `SimGpu`
//! emulator can call a shader as the plain function it is: what runs in
//! a test is the code that runs on the GPU. Image and sampler types have
//! no CPU form; a shader that samples keeps its maths in plain functions
//! ([`shade`]) that both sides call.
#![cfg_attr(target_arch = "spirv", no_std)]

pub use spirv_std;

use spirv_std::glam::{UVec3, Vec2, Vec4, vec2, vec4};
use spirv_std::image::{Image2d, SampledImage};
use spirv_std::spirv;

/// Workgroup size of the compute shaders.
pub const THREADS: u32 = 64;

/// Doubles each number: invocation `id.x` doubles `numbers[id.x]`.
#[spirv(compute(threads(64)))]
pub fn double(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] numbers: &mut [u32],
) {
    let i = id.x as usize;
    if i < numbers.len() {
        numbers[i] *= 2;
    }
}

/// The corner of a full-screen triangle for vertex `i` (0, 1, 2), in
/// [0, 2] on each axis: the triangle covers the [0, 1] square.
pub fn corner(i: u32) -> Vec2 {
    vec2(((i << 1) & 2) as f32, (i & 2) as f32)
}

/// A height, as the colour the `heights` program draws: grey, brighter
/// for higher.
pub fn shade(height: f32) -> Vec4 {
    vec4(height, height, height, 1.0)
}

/// A full-screen triangle whose colour the vertex stage reads from a
/// heightmap (binding 0, a combined image sampler) at `uv`, given as push
/// constants: a vertex shader sampling, as a terrain mesh does.
#[spirv(vertex)]
pub fn heights_vs(
    #[spirv(vertex_index)] i: u32,
    #[spirv(push_constant)] uv: &Vec2,
    #[spirv(descriptor_set = 0, binding = 0)] heights: &SampledImage<Image2d>,
    #[spirv(position)] position: &mut Vec4,
    #[spirv(flat)] color: &mut Vec4,
) {
    let p = corner(i);
    *position = vec4(p.x * 2.0 - 1.0, p.y * 2.0 - 1.0, 0.0, 1.0);
    let height: Vec4 = heights.sample_by_lod(*uv, 0.0);
    *color = shade(height.x);
}

/// Writes the colour the vertex stage chose.
#[spirv(fragment)]
pub fn heights_fs(#[spirv(flat)] color: Vec4, out: &mut Vec4) {
    *out = color;
}
