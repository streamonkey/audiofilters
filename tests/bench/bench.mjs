// Wasm counterpart of `smkaf-tools bench`: times every filter in the wasm
// module on the parity fixtures (same signal, block size and parameter
// events), through the same raw ABI the AudioWorklet uses, including the
// copy into and out of the wasm I/O buffer.
//
// usage: node tests/bench/bench.mjs <fixtures-dir> <smkaf.wasm>
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const RUNS = 5;

const [dir, wasmPath] = process.argv.slice(2);
if (!dir || !wasmPath) {
  console.error('usage: bench.mjs <fixtures-dir> <smkaf.wasm>');
  process.exit(2);
}

const b = readFileSync(join(dir, 'input.f32'));
const input = new Float32Array(b.buffer, b.byteOffset, b.byteLength / 4);
const manifest = JSON.parse(readFileSync(join(dir, 'manifest.json'), 'utf8'));
const { instance } = await WebAssembly.instantiate(readFileSync(wasmPath), {});
const ex = instance.exports;
const { rate, channels: ch, block, blocks } = manifest;
const frames = blocks * block;
const output = new Float32Array(input.length);

function run(kind, events) {
  const h = ex.smkaf_new(kind, rate, ch);
  const start = performance.now();
  let next = 0;
  for (let i = 0; i < blocks; i++) {
    while (next < events.length && events[next][0] === i) {
      const [, p, v] = events[next++];
      ex.smkaf_set_param(h, p, v);
    }
    const io = new Float32Array(ex.memory.buffer, ex.smkaf_buffer(h), block * ch);
    io.set(input.subarray(i * block * ch, (i + 1) * block * ch));
    ex.smkaf_process(h, block);
    output.set(io, i * block * ch);
    ex.smkaf_meter(h);
  }
  const ms = performance.now() - start;
  ex.smkaf_free(h);
  return ms;
}

const best = (kind, events) => Math.min(...Array.from({ length: RUNS }, () => run(kind, events)));

const audioMs = (frames / rate) * 1000;
const pad = (s, n) => String(s).padStart(n);
console.log(`wasm: ${ch} ch, ${rate} Hz, ${block}-frame blocks, ${frames / rate} s of audio, best of ${RUNS}`);
console.log(`${'filter'.padEnd(12)} ${pad('default ns/frame', 16)} ${pad('x realtime', 12)} ${pad('automated ns/fr', 16)} ${pad('x realtime', 12)}`);
for (const f of manifest.filters) {
  const fixed = best(f.kind, []);
  const automated = best(f.kind, f.events);
  const ns = (ms) => ((ms * 1e6) / frames).toFixed(1);
  const rt = (ms) => (audioMs / ms).toFixed(0);
  console.log(`${f.name.padEnd(12)} ${pad(ns(fixed), 16)} ${pad(rt(fixed), 12)} ${pad(ns(automated), 16)} ${pad(rt(automated), 12)}`);
}
