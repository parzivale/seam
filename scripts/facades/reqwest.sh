#!/bin/sh
# Regenerates crates/seam-reqwest/src/real/builder.rs from reqwest's rustdoc
# JSON, for the reqwest version in Cargo.lock.
# Needs Python 3 and a stable Rust (rustdoc's JSON output is unstable, so
# RUSTC_BOOTSTRAP turns it on).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
version=$(grep -A1 '^name = "reqwest"$' "$root/Cargo.lock" | sed -n 's/^version = "\(.*\)"$/\1/p')
work=$(mktemp -d)
mkdir -p "$work/src"
: > "$work/src/lib.rs"
cat > "$work/Cargo.toml" <<TOML
[package]
name = "reqwest-facade-json"
version = "0.0.0"
edition = "2024"
[dependencies]
reqwest = { version = "=$version", default-features = false, features = ["rustls"] }
[workspace]
TOML
(cd "$work" && RUSTC_BOOTSTRAP=1 cargo rustdoc -q -p reqwest -- -Zunstable-options --output-format json)
python3 "$here/reqwest.py" "$work/target/doc/reqwest.json" > "$root/crates/seam-reqwest/src/real/builder.rs"
rustfmt --edition 2024 "$root/crates/seam-reqwest/src/real/builder.rs"
rm -rf "$work"
echo "regenerated from reqwest $version"
