//! `Ash` as a root part, with tuned settings, next to other parts; and the
//! same core on `SimGpu`. Passes with a note where there is no Vulkan.
#![cfg(all(feature = "ash", feature = "std", feature = "env"))]

use core::time::Duration;
use seam::ash::{effects as gpu, *};
use seam::effects as fx;
use seam::ops::env::Var;
use seam::prelude::*;
use seam::stdlib::Std;

/// Uploads, submits, and reads back, asking the root for the GPU and the
/// environment alike.
fn round_trip<R, S>(root: &mut R) -> Result<Vec<u32>, GpuError>
where
    R: Provide<CreateBuffer>
        + Provide<Submit<S>>
        + for<'a> Perform<Upload<'a>>
        + for<'a> Perform<Download<'a>>
        + for<'a> Inspect<Var<'a>>,
    S: Inspect<IsDone>,
{
    let _home = root.inspect(Var::new("HOME"));
    root.acquire(CreateBuffer::new(8, BufferUsage::STORAGE), |root, buf| {
        root.perform(Upload::new(*buf, &u32_bytes(&[4, 5])))?;
        root.acquire(Submit::<S>::new(), |_, _| Ok::<_, GpuError>(()))?;
        let mut out = [0; 8];
        root.perform(Download::new(*buf, &mut out))?;
        Ok(bytes_u32(&out))
    })
}

#[test]
fn ash_with_tuned_settings_in_a_root() {
    let options = AshOptions {
        spin: Duration::from_micros(100),
        waiter_stack: 32 * 1024,
        ..AshOptions::default()
    };
    // Built in the root...
    let built = || -> Result<_, GpuError> {
        Ok(seam::root![
            Std<fx::Var>,
            Ash < gpu::CreateBuffer,
            gpu::Upload,
            gpu::Download,
            gpu::Submit > ::with(&options)?,
        ])
    };
    let mut root = match built() {
        Ok(root) => root,
        Err(e) => return eprintln!("skipping: no Vulkan device here ({e})"),
    };
    assert_eq!(round_trip::<_, Submission>(&mut root).unwrap(), [4, 5]);

    // ...or built first and shared: every clone is the same device.
    let device = Ash::with(&options).unwrap();
    let mut root = seam::root![
        Std<fx::Var>,
        Ash < gpu::CreateBuffer,
        gpu::Upload,
        gpu::Download,
        gpu::Submit > ::clone(&device),
    ];
    assert_eq!(round_trip::<_, Submission>(&mut root).unwrap(), [4, 5]);
}

#[test]
fn the_same_core_on_the_simulated_gpu() {
    let mut root = seam::root![
        Std<fx::Var>,
        SimGpu < gpu::CreateBuffer,
        gpu::Upload,
        gpu::Download,
        gpu::Submit > ::new(),
    ];
    assert_eq!(round_trip::<_, SimSubmission>(&mut root).unwrap(), [4, 5]);
}
