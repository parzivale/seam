//! nix against the real system.
#![cfg(unix)]

include!("../../seam-unix/tests/common/real.rs");

#[test]
fn nix_performs_the_unix_effects() {
    exercise(&mut seam_nix::Nix, "nix");
}

/// Both handlers see the same system.
#[test]
fn nix_and_rustix_agree() {
    use seam_core::Inspect;
    use seam_unix::{Hostname, Ids, ProcessId};
    let (n, r) = (seam_nix::Nix, seam_rustix::Rustix);
    assert_eq!(n.inspect(Ids), r.inspect(Ids));
    assert_eq!(n.inspect(ProcessId), r.inspect(ProcessId));
    assert_eq!(n.inspect(Hostname).unwrap(), r.inspect(Hostname).unwrap());
}
