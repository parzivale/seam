//! `Sim`'s world-file blocks go through the same machinery as any other
//! crate's simulated part: this compiles their source (`src/sim_world.rs`)
//! here, outside seam-test, over a stand-in `Sim` that reaches the real one
//! only through its public API. A private item used there fails this build.
#![cfg(feature = "full")]

use seam_test::world_file;

/// The stand-in the module's `super::Sim` resolves to.
struct Sim(seam_test::Sim);

impl core::ops::Deref for Sim {
    type Target = seam_test::Sim;
    fn deref(&self) -> &seam_test::Sim {
        &self.0
    }
}

impl core::ops::DerefMut for Sim {
    fn deref_mut(&mut self) -> &mut seam_test::Sim {
        &mut self.0
    }
}

#[path = "../src/sim_world.rs"]
mod sim_world;

/// The copy built here behaves as the real one.
#[test]
fn the_outside_copy_loads_and_checks() {
    let world = r#"
        clock now=3
        env { var USER ada }
        fs { file "/a.txt" "hi" }
        stdio { stdin "y\n" }
    "#;
    let mut outside = Sim(seam_test::Sim::new());
    world_file::load(&mut outside, world).unwrap();
    let inside = seam_test::Sim::from_world(world).unwrap();
    for sim in [&outside.0, &inside] {
        assert_eq!(sim.file("/a.txt").unwrap(), b"hi");
    }
    world_file::check(&outside, "clock now=3\nfs { file \"/a.txt\" \"hi\" }").unwrap();
    let err = world_file::check(&outside, "env { var USER bob }").unwrap_err();
    assert_eq!(err.failures.len(), 1);
}
