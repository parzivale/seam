//! rustix against the real system.
#![cfg(unix)]

include!("../../seam-unix/tests/common/real.rs");

#[test]
fn rustix_performs_the_unix_effects() {
    exercise(&mut seam_rustix::Rustix, "rustix");
}
