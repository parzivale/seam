//! Compiles the shader crate to SPIR-V, and writes it next to the crate as
//! `shaders.spv`.

use std::path::Path;

use spirv_builder::{ModuleResult, SpirvBuilder};

/// The nightly rust-gpu's backend is built for, for the inner build of the
/// shader crate (which runs in that crate's directory, outside this
/// toolchain file's reach).
const TOOLCHAIN: &str = "nightly-2026-08-15";

fn main() {
    let shaders = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut builder = SpirvBuilder::new(&shaders, "spirv-unknown-vulkan1.2");
    builder.toolchain_overwrite = Some(TOOLCHAIN.into());
    let built = builder.build().expect("the shaders compile");
    let ModuleResult::SingleModule(spv) = built.module else {
        panic!("expected one module");
    };
    let out = shaders.join("shaders.spv");
    std::fs::copy(&spv, &out).expect("the module is written");
    println!("wrote {} ({} entry points)", out.display(), built.entry_points.len());
}
