{ lib, rustPlatform, pkg-config, gst_all_1 }:

let
  root = ../..;
in
rustPlatform.buildRustPackage {
  pname = "gst-plugin-smkaf";
  version = (lib.importTOML (root + "/Cargo.toml")).workspace.package.version;

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (root + "/Cargo.toml")
      (root + "/Cargo.lock")
      (root + "/crates")
    ];
  };
  cargoLock.lockFile = root + "/Cargo.lock";

  cargoBuildFlags = [ "-p" "gst-plugin-smkaf" ];
  cargoTestFlags = [ "-p" "smkaf-core" "-p" "gst-plugin-smkaf" ];

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ gst_all_1.gstreamer gst_all_1.gst-plugins-base ];

  # GStreamer wants a writable registry cache.
  preCheck = ''
    export HOME=$TMPDIR
  '';

  postInstall = ''
    mkdir -p $out/lib/gstreamer-1.0
    mv $out/lib/libgstsmkaf.so $out/lib/gstreamer-1.0/
  '';

  meta = {
    description = "streaMonkey audio filters for GStreamer";
    homepage = "https://github.com/streamonkey/audiofilters";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
  };
}
