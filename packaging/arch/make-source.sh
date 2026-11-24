#!/bin/sh
# Create packaging/arch/smkaf-<version>.tar.gz with everything the PKGBUILD
# needs (the Cargo workspace and the license).
set -eu
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
out=packaging/arch/smkaf-$version.tar.gz
tar --exclude=target --transform "s,^,smkaf-$version/," -czf "$out" Cargo.toml Cargo.lock LICENSE crates
echo "$out"
