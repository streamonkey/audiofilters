#!/bin/sh
# Set the release version in all places that carry it:
# Cargo workspace, npm package and PKGBUILD.
set -eu
[ $# -eq 1 ] || { echo "usage: $0 X.Y.Z" >&2; exit 2; }
v=$1
cd "$(dirname "$0")/.."

sed -i "0,/^version = \".*\"/s//version = \"$v\"/" Cargo.toml
sed -i "s/^pkgver=.*/pkgver=$v/; s/^pkgrel=.*/pkgrel=1/" packaging/arch/PKGBUILD
(cd npm && npm version --no-git-tag-version --allow-same-version "$v" >/dev/null)
cargo update --workspace --offline

echo "version set to $v; commit and tag with v$v"
