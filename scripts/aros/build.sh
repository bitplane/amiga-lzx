#!/bin/sh
# Cross-compile the lzx CLI for AROS in a Mountin compiler toolbox.
# Usage: build.sh <x86_64-aros|i386-aros|aarch64-aros>
set -eu
root=$(CDPATH='' cd "$(dirname "$0")/../.." && pwd)
target=${1:?usage: build.sh <x86_64-aros|i386-aros|aarch64-aros>}
case "$target" in
    x86_64-aros|i386-aros|aarch64-aros) ;;
    *) echo "Unknown AROS target: $target" >&2; exit 1 ;;
esac
image=ghcr.io/bitplane/mountin/builder/compiler/aros/${target%-aros}:v0.1.14

# The toolbox sets the Cargo target, linker and panic strategy, and patches
# crates.io's libc (which has no AROS module) with the port's own through
# its CARGO_HOME. Cargo.lock pins the registry libc, so the build re-locks
# onto the patch and restores the committed lock file afterwards.
exec "${CONTAINER:-docker}" run --rm \
    --volume "$root:/work" \
    --workdir /work \
    "$image" \
    sh -euc '
        cp Cargo.lock "$CARGO_HOME/Cargo.lock.orig"
        trap "cp \"\$CARGO_HOME/Cargo.lock.orig\" Cargo.lock" EXIT
        libc=$(sed -n "s/^libc = { path = \"\(.*\)\" }$/\1/p" "$CARGO_HOME/config.toml")
        version=$(sed -n "s/^version = \"\(.*\)\"$/\1/p" "$libc/Cargo.toml" | head -n 1)
        cargo update --package libc --precise "$version"
        cargo build --release --package amiga-lzx-cli
    '
