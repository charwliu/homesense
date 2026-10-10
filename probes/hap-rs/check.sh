#!/usr/bin/env bash
set -euo pipefail
probe_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source_repo="${1:-https://github.com/ewilken/hap-rs.git}"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/homesense-hap-check.XXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT
git clone --quiet --no-hardlinks "$source_repo" "$scratch/source"
git -C "$scratch/source" checkout --quiet --detach acf45bdb00e23c81b67b3d5116c113335dc83f16
git -C "$scratch/source" apply "$probe_dir/candidate.patch"
cp "$probe_dir/Cargo.lock" "$scratch/source/Cargo.lock"
mkdir -p "$scratch/source/tests"
cp "$probe_dir/homesense_contract.rs" "$scratch/source/tests/homesense_contract.rs"
export CARGO_TARGET_DIR="$probe_dir/../../target/hap-candidate"
cargo check --offline --locked --manifest-path "$scratch/source/Cargo.toml" --example lightbulb
cargo test --offline --locked --manifest-path "$scratch/source/Cargo.toml" --lib --test homesense_contract
