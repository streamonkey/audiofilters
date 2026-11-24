#!/bin/sh
# Check that a release tag (vX.Y.Z) matches the version in all places that
# carry it (see set-version.sh).
set -eu
[ $# -eq 1 ] || { echo "usage: $0 vX.Y.Z" >&2; exit 2; }
v=${1#v}
cd "$(dirname "$0")/.."

cargo=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
npm=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' npm/package.json)
arch=$(sed -n 's/^pkgver=//p' packaging/arch/PKGBUILD)

ok=true
for pair in "Cargo.toml:$cargo" "npm/package.json:$npm" "PKGBUILD:$arch"; do
  if [ "${pair#*:}" != "$v" ]; then
    echo "${pair%%:*} has version ${pair#*:}, tag says $v" >&2
    ok=false
  fi
done
$ok
