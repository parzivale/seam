#!/bin/sh
# Regenerates crates/seam-jiff/src/jiff_facade.rs from jiff's rustdoc JSON,
# for the jiff version in Cargo.lock, built with seam-jiff's features.
# Needs Python 3 and a stable Rust (rustdoc's JSON output is unstable, so
# RUSTC_BOOTSTRAP turns it on).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
version=$(grep -A1 '^name = "jiff"$' "$root/Cargo.lock" | sed -n 's/^version = "\(.*\)"$/\1/p')
work=$(mktemp -d)
mkdir -p "$work/src"
: > "$work/src/lib.rs"
cat > "$work/Cargo.toml" <<TOML
[package]
name = "jiff-facade-json"
version = "0.0.0"
edition = "2024"
[dependencies]
jiff = { version = "=$version", default-features = false, features = ["std"] }
[workspace]
TOML
(cd "$work" && RUSTC_BOOTSTRAP=1 cargo rustdoc -q -p jiff -- -Zunstable-options --output-format json)
python3 "$here/jiff.py" "$work/target/doc/jiff.json" > "$root/crates/seam-jiff/src/jiff_facade.rs"
rustfmt --edition 2024 "$root/crates/seam-jiff/src/jiff_facade.rs"
rm -rf "$work"
echo "regenerated from jiff $version"
