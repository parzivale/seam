//! A compute shader written in Rust (rust-gpu), run on the GPU through
//! `seam::ash` and, in the tests, in `SimGpu` as the plain Rust function it
//! also is.
//!
//! The shader is `double` in `crates/seam-ash/shaders`, compiled to
//! `shaders.spv` by that crate's `build` directory. The program runs it on
//! Vulkan where there is a device, and in the simulation otherwise.
//!
//! ```text
//! cargo run -p seam-rs --example gpu_double --features std,test,ash
//! ```

use seam::ash::*;
use seam::prelude::*;
use seam_ash_shaders as shaders;
use shaders::spirv_std::glam::UVec3;

/// The shader's SPIR-V, as rust-gpu wrote it.
fn spirv() -> Vec<u32> {
    bytes_u32(include_bytes!("../../seam-ash/shaders/shaders.spv"))
}

/// The core: doubles `numbers` on the GPU.
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

/// A simulated GPU that runs `double` by calling the shader itself, one
/// invocation at a time.
fn simulated() -> SimGpu {
    let mut gpu = SimGpu::new();
    gpu.emulate("double", |run| {
        let mut numbers = bytes_u32(run.buffer(0));
        for x in 0..run.groups()[0] * shaders::THREADS {
            shaders::double(UVec3::new(x, 0, 0), &mut numbers);
        }
        run.buffer(0).copy_from_slice(&u32_bytes(&numbers));
    });
    gpu
}

/// The edge: Vulkan if there is a device, the simulation if not.
fn main() -> Result<(), GpuError> {
    let numbers: Vec<u32> = (1..=10).collect();
    let doubled = match Ash::new() {
        Ok(mut gpu) => {
            println!("on {}", gpu.inspect(DeviceInfo).name);
            double(&mut gpu, &spirv(), &numbers)?
        }
        Err(e) => {
            println!("no Vulkan device ({e}); in the simulation");
            double(&mut simulated(), &[], &numbers)?
        }
    };
    println!("{numbers:?}\n-> {doubled:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core, and the shader's own code, in the simulation: no GPU.
    #[test]
    fn the_shader_doubles_in_the_simulation() {
        let numbers: Vec<u32> = (0..200).collect();
        let doubled = double(&mut simulated(), &[], &numbers).unwrap();
        assert_eq!(doubled, numbers.iter().map(|n| n * 2).collect::<Vec<_>>());
    }
}
