//! `smkaf-tools metadata`        print the filter descriptors as JSON
//! `smkaf-tools fixtures <dir>`  write native reference output for the
//!                               native-vs-wasm parity test
//! `smkaf-tools bench`           time every filter natively (ns per frame)

use std::fmt::Write as _;
use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};
use std::{env, fs, process};

use smkaf_core::{GraphKind, ParamKind, FILTERS};

/// At most `smkaf_wasm::MAX_FRAMES`, the largest block `smkaf_process` takes.
const BLOCK: usize = 128;
const RATE: f32 = 48000.0;
const CHANNELS: usize = 2;
const SECONDS: usize = 4;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["metadata"] => print!("{}", metadata()),
        ["fixtures", dir] => fixtures(Path::new(dir)),
        ["bench"] => bench(),
        _ => {
            eprintln!("usage: smkaf-tools metadata | fixtures <dir> | bench");
            process::exit(2);
        }
    }
}

fn json_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn metadata() -> String {
    let mut out = String::from("[\n");
    for (k, d) in FILTERS.iter().enumerate() {
        let graph = match d.graph {
            GraphKind::None => "null",
            GraphKind::Transfer => "\"transfer\"",
            GraphKind::Frequency => "\"frequency\"",
        };
        let meter = match d.meter {
            Some((name, unit)) => format!("{{\"name\": {}, \"unit\": {}}}", json_str(name), json_str(unit)),
            None => "null".into(),
        };
        let _ = writeln!(out, "  {{");
        let _ = writeln!(out, "    \"name\": {},", json_str(d.name));
        let _ = writeln!(out, "    \"label\": {},", json_str(d.label));
        let _ = writeln!(out, "    \"description\": {},", json_str(d.description));
        let _ = writeln!(out, "    \"graph\": {graph},");
        let _ = writeln!(out, "    \"meter\": {meter},");
        let _ = writeln!(out, "    \"params\": [");
        for (i, p) in d.params.iter().enumerate() {
            let (kind, options) = match p.kind {
                ParamKind::Float => ("float", String::new()),
                ParamKind::Bool => ("bool", String::new()),
                ParamKind::Choice(o) => {
                    let o: Vec<String> = o.iter().map(|s| json_str(s)).collect();
                    ("choice", format!(", \"options\": [{}]", o.join(", ")))
                }
            };
            let sep = if i + 1 < d.params.len() { "," } else { "" };
            let _ = writeln!(
                out,
                "      {{\"name\": {}, \"label\": {}, \"unit\": {}, \"kind\": \"{kind}\", \"min\": {:?}, \"max\": {:?}, \"default\": {:?}{options}}}{sep}",
                json_str(p.name),
                json_str(p.label),
                json_str(p.unit),
                p.min,
                p.max,
                p.default,
            );
        }
        let _ = writeln!(out, "    ]");
        let sep = if k + 1 < FILTERS.len() { "," } else { "" };
        let _ = writeln!(out, "  }}{sep}");
    }
    out.push_str("]\n");
    out
}

/// Deterministic PRNG (xorshift32), so fixtures are reproducible.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 8) as f32 / (1u32 << 24) as f32
    }
}

/// Test signal: alternating sections of loud noise, sine bursts, a quiet
/// passage, silence and a full-scale step, with different content per
/// channel.
fn test_signal() -> Vec<f32> {
    let frames = RATE as usize * SECONDS;
    let section = RATE as usize / 4;
    let mut rng = Rng(0x1234_5678);
    let mut out = Vec::with_capacity(frames * CHANNELS);
    for i in 0..frames {
        let t = i as f32 / RATE;
        for c in 0..CHANNELS {
            let noise = rng.unit() * 2.0 - 1.0;
            let sine = (2.0 * std::f32::consts::PI * (220.0 + 2500.0 * c as f32) * t).sin();
            let s = match (i / section) % 6 {
                0 => noise * 0.9,
                1 => sine * 0.7 + noise * 0.05,
                2 => sine * 0.01,
                3 => 0.0,
                4 => if c == 0 { 1.0 } else { -0.5 },
                _ => noise * 0.3 + sine * 0.3,
            };
            out.push(s);
        }
    }
    out
}

fn write_f32(path: &Path, data: &[f32]) {
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
    fs::write(path, bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

/// Parameter events `(block, param, value)` for filter `k`: every 25 blocks
/// a random parameter gets a random value in its range.
fn param_events(k: usize, blocks: usize) -> Vec<(usize, usize, f32)> {
    let desc = FILTERS[k];
    let mut rng = Rng(0x9e37_79b9 ^ (k as u32 + 1));
    let mut events = Vec::new();
    for b in (10..blocks).step_by(25) {
        let p = (rng.next() as usize) % desc.params.len();
        let info = &desc.params[p];
        events.push((b, p, info.min + rng.unit() * (info.max - info.min)));
    }
    events
}

/// Process `buf` in place through a fresh instance of filter `k` in `BLOCK`
/// sized blocks, applying `events`. Returns the meter value after each block.
fn run(k: usize, buf: &mut [f32], events: &[(usize, usize, f32)]) -> Vec<f32> {
    let mut filter = smkaf_core::create(FILTERS[k], RATE, CHANNELS);
    let mut meters = Vec::with_capacity(buf.len() / (BLOCK * CHANNELS));
    let mut next_event = 0;
    for (b, chunk) in buf.chunks_exact_mut(BLOCK * CHANNELS).enumerate() {
        while next_event < events.len() && events[next_event].0 == b {
            let (_, p, v) = events[next_event];
            filter.set_param(p, v);
            next_event += 1;
        }
        filter.process(chunk);
        meters.push(filter.meter());
    }
    meters
}

/// Best of this many runs is reported, to filter out scheduling noise.
const BENCH_RUNS: usize = 5;

/// Time every filter on the fixture signal, once with default parameters
/// and once with the fixture parameter events (smoothing, coefficient
/// updates, graphic EQ solves). Same signal, block size and events as
/// `tests/bench/bench.mjs` uses for the wasm build.
fn bench() {
    let input = test_signal();
    let frames = input.len() / CHANNELS;
    let blocks = frames / BLOCK;
    let mut buf = input.clone();
    let mut time = |k: usize, events: &[(usize, usize, f32)]| {
        (0..BENCH_RUNS)
            .map(|_| {
                buf.copy_from_slice(&input);
                let start = Instant::now();
                black_box(run(k, black_box(&mut buf), events));
                start.elapsed()
            })
            .min()
            .unwrap()
    };
    print_bench_header("native", frames);
    for (k, desc) in FILTERS.iter().enumerate() {
        let fixed = time(k, &[]);
        let automated = time(k, &param_events(k, blocks));
        print_bench_row(desc.name, frames, fixed, automated);
    }
}

fn print_bench_header(target: &str, frames: usize) {
    println!(
        "{target}: {CHANNELS} ch, {RATE} Hz, {BLOCK}-frame blocks, {:.0} s of audio, best of {BENCH_RUNS}",
        frames as f32 / RATE
    );
    println!("{:<12} {:>16} {:>12} {:>16} {:>12}", "filter", "default ns/frame", "x realtime", "automated ns/fr", "x realtime");
}

fn print_bench_row(name: &str, frames: usize, fixed: Duration, automated: Duration) {
    let audio = frames as f64 / RATE as f64;
    let ns = |d: Duration| d.as_nanos() as f64 / frames as f64;
    let rt = |d: Duration| audio / d.as_secs_f64();
    println!(
        "{name:<12} {:>16.1} {:>12.0} {:>16.1} {:>12.0}",
        ns(fixed),
        rt(fixed),
        ns(automated),
        rt(automated)
    );
}

fn fixtures(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    let input = test_signal();
    write_f32(&dir.join("input.f32"), &input);
    let blocks = input.len() / (BLOCK * CHANNELS);

    let mut manifest = format!(
        "{{\n  \"rate\": {RATE:?},\n  \"channels\": {CHANNELS},\n  \"block\": {BLOCK},\n  \"blocks\": {blocks},\n  \"filters\": [\n"
    );
    for (k, desc) in FILTERS.iter().enumerate() {
        let events = param_events(k, blocks);
        let mut output = input.clone();
        let meters = run(k, &mut output, &events);
        write_f32(&dir.join(format!("{}.expected.f32", desc.name)), &output);
        write_f32(&dir.join(format!("{}.meter.f32", desc.name)), &meters);

        let ev: Vec<String> = events.iter().map(|(b, p, v)| format!("[{b}, {p}, {v:?}]")).collect();
        let sep = if k + 1 < FILTERS.len() { "," } else { "" };
        let _ = writeln!(
            manifest,
            "    {{\"kind\": {k}, \"name\": {}, \"events\": [{}]}}{sep}",
            json_str(desc.name),
            ev.join(", ")
        );
    }
    manifest.push_str("  ]\n}\n");
    fs::write(dir.join("manifest.json"), manifest).unwrap();
    eprintln!("wrote fixtures for {} filters to {}", FILTERS.len(), dir.display());
}
