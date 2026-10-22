#!/bin/sh
# Package a built AROS lzx as a Pkg-ready archive in dist/.
# Usage: package.sh <x86_64-aros|i386-aros|aarch64-aros> <version>
set -eu
umask 022
root=$(CDPATH='' cd "$(dirname "$0")/../.." && pwd)
target=${1:?usage: package.sh <target> <version>}
version=${2:?usage: package.sh <target> <version>}
case "$target" in
    x86_64-aros) triple=x86_64-unknown-aros ;;
    i386-aros) triple=i686-unknown-aros ;;
    aarch64-aros) triple=aarch64-unknown-aros ;;
    *) echo "Unknown AROS target: $target" >&2; exit 1 ;;
esac
case "$version" in ''|*[!0-9.]*) echo "Invalid version: $version" >&2; exit 1 ;; esac
# Every architecture shares the release commit's timestamp.
epoch=$(git -C "$root" log -1 --format=%ct)
mkdir -p "$root/dist"
work=$(mktemp -d "$root/dist/.package-aros.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
package=lzx
mkdir -p "$work/$package/C" "$work/$package/Help/$package"
# Not stripped: the AROS loader resolves relocations through the symbol
# table, and even removing unallocated sections with objcopy breaks it.
cp "$root/target/$triple/release/lzx" "$work/$package/C/lzx"
chmod 755 "$work/$package/C/lzx"
cp "$root/README.md" "$work/$package/Help/$package/README.md"
cp "$root/LICENSE" "$work/$package/Help/$package/LICENSE"
chmod 644 "$work/$package/Help/$package/"*
archive="$root/dist/$package-$version-$target.tar.bz2"
tar -C "$work" --sort=name --mtime="@$epoch" --owner=0 --group=0 \
    --numeric-owner -cjf "$archive" "$package"
echo "$archive"
