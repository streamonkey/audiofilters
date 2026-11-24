#!/bin/sh
# Runs inside archlinux:base-devel (see `make arch-docker`): build the package
# with makepkg as an unprivileged user, install it, check that GStreamer finds
# the plugin and copy the package back to the mounted /pkg.
set -eu

pacman -Syu --noconfirm --needed cargo gstreamer gst-plugins-base-libs

useradd -m builder
install -d -o builder /home/builder/pkg
install -o builder /pkg/PKGBUILD /pkg/smkaf-*.tar.gz /home/builder/pkg/
su builder -c 'cd ~/pkg && makepkg --noconfirm'

pacman -U --noconfirm /home/builder/pkg/*.pkg.tar.zst
gst-inspect-1.0 smkaf

install -o "$HOST_UID" -g "$HOST_GID" -m 644 /home/builder/pkg/*.pkg.tar.zst /pkg/
