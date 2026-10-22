#!/bin/sh
# Sign an AROS release archive and push it to the bitplane Pkg channel.
# Usage: PKG_SIGNKEY=<key file> publish.sh <archive> <release-tag>
set -eu
archive=${1:?usage: publish.sh <archive> <release-tag>}
tag=${2:?usage: publish.sh <archive> <release-tag>}
version=${tag#v}
case "$tag" in v[0-9]*) ;; *) echo "Expected a release tag beginning with v" >&2; exit 1 ;; esac
case "$version" in ''|*[!0-9.]*) echo "Invalid release version" >&2; exit 1 ;; esac
package=lzx
name=$(basename "$archive")
case "$name" in
    "$package-$version-i386-aros.tar.bz2") arch=i386 ;;
    "$package-$version-aarch64-aros.tar.bz2") arch=aarch64 ;;
    "$package-$version-x86_64-aros.tar.bz2") arch=x86_64 ;;
    *) echo "Archive name does not match the release or an AROS target" >&2; exit 1 ;;
esac
pkg=${PKG:-pkg}
: "${PKG_SIGNKEY:?Set PKG_SIGNKEY to your signing key file}"
repo=${GITHUB_REPOSITORY:-bitplane/amiga-lzx}
channel_url=${PKG_CHANNEL_URL:-https://aros-pkg.azurewebsites.net/bitplane}
archive=$(CDPATH='' cd "$(dirname "$archive")" && pwd)/$name
work=$(mktemp -d "$(dirname "$archive")/.publish-aros.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
upstream="https://github.com/$repo/releases/download/$tag/$name"
kind=application
short='Amiga LZX archiver'
source="$archive!/$package"
"$pkg" MANIFEST "$source" KIND "$kind" > "$work/manifest"
for field in "Name: $package" "Version: $version" "Architecture: $arch"; do
    grep -Fxq "$field" "$work/manifest" || { echo "Unexpected package metadata: expected $field" >&2; exit 1; }
done
"$pkg" PUBLISH "$source" CHANNEL "$work/channel" KIND "$kind" \
    UPSTREAM "$upstream" SHORT "$short" \
    HOMEPAGE "https://bitplane.net/dev/rust/amiga-lzx" REPOSITORY "https://github.com/$repo" \
    LICENSE WTFPL DISTRIBUTION open-source
"$pkg" PUSH CHANNEL "$work/channel" TO "$channel_url"
