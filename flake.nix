{
  description = "smkaf - streaMonkey audio filters (GStreamer plugin + WebAudio/WASM)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        gst-plugin-smkaf = pkgs.callPackage ./packaging/nix/package.nix { };
        default = gst-plugin-smkaf;
      });

      overlays.default = final: prev: {
        gst-plugin-smkaf = final.callPackage ./packaging/nix/package.nix { };
      };

      devShells = forAllSystems (pkgs:
        let gst = pkgs.gst_all_1; in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = [
              # nixpkgs' rustc ships the wasm32-unknown-unknown std as well;
              # lld links the wasm module.
              pkgs.rustc
              pkgs.cargo
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
              pkgs.lld
              pkgs.pkg-config

              pkgs.nodejs
              pkgs.binaryen # wasm-opt
              pkgs.gnumake
            ];
            # Libraries go here so the pkg-config and GStreamer setup hooks
            # pick them up (PKG_CONFIG_PATH, GST_PLUGIN_SYSTEM_PATH_1_0).
            buildInputs = [
              gst.gstreamer
              gst.gst-plugins-base
              gst.gst-plugins-good # for testing pipelines (wavparse, autoaudiosink, ...)
            ];
            # nixpkgs' rustc has no rust-src component; rust-analyzer needs the
            # standard library sources.
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";

            shellHook = ''
              # Let gst-inspect-1.0 / gst-launch-1.0 find the freshly built plugin.
              export GST_PLUGIN_PATH="$PWD/target/release:$PWD/target/debug''${GST_PLUGIN_PATH:+:$GST_PLUGIN_PATH}"
            '';
          };
        });
    };
}
