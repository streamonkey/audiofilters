# smkaf — streaMonkey audio filters

A collection of audio filters for radio and playout use. The same Rust DSP code
runs in two places:

- **GStreamer**: plugin `smkaf`, shipped as the Arch package `gst-plugin-smkaf`
- **Browser**: npm package `@streamonkey/audiofilters` (WebAssembly + AudioWorklet)

Both builds produce **bit-identical output** for the same input and parameter
changes. CI checks this on every run.

| Filter | GStreamer element | What it does | Meter |
|---|---|---|---|
| `compressor` | `smkafcompressor` | Soft-knee feed-forward compressor: peak/RMS detector, stereo link, dry/wet mix | gain reduction (dB) |
| `limiter` | `smkaflimiter` | Look-ahead brickwall limiter, always stereo-linked (never exceeds the ceiling, latency is reported) | gain reduction (dB) |
| `gate` | `smkafgate` | Noise gate with hysteresis, hold and range | gain (dB) |
| `eq` | `smkafeq` | High-pass 12/24 dB/oct, low shelf, 3 peaking bands, high shelf, low-pass | – |
| `graphiceq` | `smkafgraphiceq` | 20-band graphic EQ (22 Hz–16 kHz, ±12 dB); filter gains are solved so the curve passes through the sliders | – |
| `deesser` | `smkafdeesser` | Split-band or wideband de-esser, with a sidechain listen mode | gain reduction (dB) |
| `leveler` | `smkafleveler` | Slow AGC towards a target level; freezes its gain during pauses | gain (dB) |
| `stereo` | `smkafstereo` | Gain, width, balance, mono, channel swap, polarity | phase correlation |

## Layout

```text
flake.nix                    dev shell (Rust + wasm32, GStreamer, Node, binaryen)
crates/smkaf-core/           #![no_std] DSP, one module per filter + parameter metadata
crates/gst-plugin-smkaf/     GStreamer plugin (libgstsmkaf.so), elements generated from the metadata
crates/smkaf-wasm/           raw extern "C" wasm ABI (no wasm-bindgen, no imports)
crates/smkaf-tools/          metadata export (for npm), native parity fixtures, native benchmark
npm/                         @streamonkey/audiofilters: TS API, worklet, build script
demo/                        Vite playground for trying the filters in the browser (testing only)
packaging/arch/              PKGBUILD, source tarball script, container build
packaging/nix/               Nix package (flake output and overlay)
tests/                       native-vs-wasm parity test, worklet glue test, wasm benchmark
```

## Development

```sh
nix develop          # or: direnv with "use flake"
make test            # Rust unit + GStreamer harness tests, wasm parity, worklet test
make gst             # target/release/libgstsmkaf.so
make npm             # npm/dist
make demo            # playground at http://localhost:5173
make bench           # per-filter ns/frame, native and wasm (bench-native: native only)
```

Inside a git repository, Nix only sees files that are tracked or staged.

### GStreamer

`nix develop` puts `target/release` and `target/debug` on `GST_PLUGIN_PATH`:

```sh
gst-inspect-1.0 smkafcompressor
gst-launch-1.0 filesrc location=in.wav ! wavparse ! audioconvert \
  ! smkafeq hp-freq=80 band3-gain=2 \
  ! smkafleveler ! smkafcompressor threshold=-24 ratio=3 makeup=4 \
  ! smkafdeesser ! smkaflimiter ceiling=-1 \
  ! audioconvert ! autoaudiosink
```

Elements accept interleaved `F32LE` at any sample rate and channel count.
`smkaflimiter` delays the audio by its look-ahead. As in `scaletempo`, it
drops the look-ahead's start-up silence, stamps output with the time of the
audio it contains (input time minus latency) and reports the latency through
the latency query. It also pushes the held-back tail out at EOS, so offline
output is sample-aligned with the input and has the same length.
Floats and booleans are controllable (`GstController`) and can be changed
while playing. Choice parameters are strings, for example `detector=rms` or
`hp-slope=12db`. Meters are read-only properties, updated once per buffer.

### Browser

```ts
import { AudioFilters } from '@streamonkey/audiofilters';

const ctx = new AudioContext();
const filters = await AudioFilters.load(ctx);
const comp = filters.create('compressor', { params: { threshold: -24, ratio: 3 } });
const limiter = filters.create('limiter', { params: { ceiling: -1 } });
source.connect(comp.node).connect(limiter.node).connect(ctx.destination);

comp.set('detector', 'rms');                 // parameter names and values are typed
comp.onmeter = (gr) => console.log(gr);      // gain reduction in dB, about every 10 ms
const curve = comp.graph([-60, -40, -20, 0]); // static curve, same code as the audio path
comp.param('threshold');                     // the AudioParam, for automation
```

Each filter is a k-rate `AudioWorkletNode` whose parameters are `AudioParam`s.
`graph()` is computed by a main-thread instance of the wasm module and follows
values set through `set()`. It does not follow AudioParam automation.

The package locates `worklet.js` and `smkaf.wasm` with
`new URL('./…', import.meta.url)`, which Vite 8 handles without any
configuration, in dev mode (including dependency pre-bundling) and in
production builds. `AudioFilters.load()` also accepts `wasmUrl` / `workletUrl`
to override the locations.

## Releasing

```sh
scripts/set-version.sh 0.2.0   # Cargo workspace, npm/package.json, PKGBUILD
git commit -am 'Release 0.2.0' && git tag v0.2.0 && git push --tags
```

A `vX.Y.Z` tag runs the release workflow. It checks that the tag matches all
version fields, publishes `@streamonkey/audiofilters` to npmjs.com (repository
secret `NPM_TOKEN`), builds the Arch package and creates a GitHub release with
the package attached.

### Arch package

```sh
make arch-src                 # packaging/arch/smkaf-<version>.tar.gz
cd packaging/arch && makepkg  # gst-plugin-smkaf-<version>-1-x86_64.pkg.tar.zst
```

Or build it in a clean `archlinux:base-devel` container, which also installs
the package and checks that `gst-inspect-1.0 smkaf` finds the plugin:

```sh
make arch-docker              # DOCKER=podman make arch-docker works too
```

This installs `/usr/lib/gstreamer-1.0/libgstsmkaf.so`.

Every GitHub release has the package attached, so an Arch-based Docker image
(in any CI) installs it with one line. `pacman -U` pulls the dependencies from
the Arch repositories:

```dockerfile
FROM archlinux:base
ARG SMKAF_VERSION=0.1.0
RUN pacman -Syu --noconfirm \
 && pacman -U --noconfirm https://github.com/streamonkey/audiofilters/releases/download/v${SMKAF_VERSION}/gst-plugin-smkaf-${SMKAF_VERSION}-1-x86_64.pkg.tar.zst \
 && pacman -Scc --noconfirm
```

Keep the `-Syu`: the package is built against the Arch repositories at release
time, and Arch does not support partial upgrades.

### NixOS

The flake provides the plugin as `packages.<system>.gst-plugin-smkaf` and as
`overlays.default`:

```sh
nix build .#gst-plugin-smkaf
GST_PLUGIN_PATH=$PWD/result/lib/gstreamer-1.0 gst-inspect-1.0 smkaf
```

System-wide, in a flake-based NixOS configuration:

```nix
inputs.smkaf.url = "github:streamonkey/audiofilters";

# in the NixOS module
nixpkgs.overlays = [ inputs.smkaf.overlays.default ];
environment.systemPackages = [ pkgs.gst-plugin-smkaf ];
```

NixOS adds `/run/current-system/sw/lib/gstreamer-1.0` to
`GST_PLUGIN_SYSTEM_PATH_1_0`, so GStreamer applications find the elements
after a re-login.

## Adding a filter

1. Add `crates/smkaf-core/src/<name>.rs` with `PARAMS`, `DESC` and an
   implementation of `Filter`. Use `compressor.rs` as the template, and add
   unit tests.
2. Register it: `pub mod` and add it to `FILTERS` in `lib.rs`.
3. Add one line to `filter_elements!` in `crates/gst-plugin-smkaf/src/lib.rs`.

Everything else follows from the metadata: GStreamer properties, wasm dispatch,
AudioParams, TypeScript types, the playground UI and the parity tests.

### Rules for `smkaf-core`, which keep native and wasm identical

- All math functions go through `libm` (`libm::exp`, not `f64::exp`).
- No SIMD, no `mul_add`, no fast-math.
- Smooth parameter changes per sample inside the filter. When a filter needs
  a control rate, count it inside the filter (see `eq.rs`), never per
  `process()` call. Then the GStreamer buffer size and the 128-frame worklet
  quantum cannot change the result.
- Flush recursive state that decays towards zero (`util::flush`).

## License

MIT, see [LICENSE](LICENSE).
