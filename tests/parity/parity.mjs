// Native vs. wasm parity test.
//
// Runs the fixtures written by `smkaf-tools fixtures <dir>` (native build)
// through the wasm module with the same block size and parameter events,
// using the same raw ABI as the AudioWorklet, and requires bit-identical
// output and meter values.
//
// usage: node tests/parity/parity.mjs <fixtures-dir> <smkaf.wasm>
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const [dir, wasmPath] = process.argv.slice(2);
if (!dir || !wasmPath) {
  console.error('usage: parity.mjs <fixtures-dir> <smkaf.wasm>');
  process.exit(2);
}

const readF32 = (name) => {
  const b = readFileSync(join(dir, name));
  return new Float32Array(b.buffer, b.byteOffset, b.byteLength / 4);
};

const manifest = JSON.parse(readFileSync(join(dir, 'manifest.json'), 'utf8'));
const input = readF32('input.f32');
const { instance } = await WebAssembly.instantiate(readFileSync(wasmPath), {});
const ex = instance.exports;
const { rate, channels: ch, block, blocks } = manifest;

let failed = 0;
for (const f of manifest.filters) {
  const expected = readF32(`${f.name}.expected.f32`);
  const expectedMeter = readF32(`${f.name}.meter.f32`);
  const output = new Float32Array(input.length);
  const meter = new Float32Array(blocks);

  const h = ex.smkaf_new(f.kind, rate, ch);
  let next = 0;
  for (let b = 0; b < blocks; b++) {
    while (next < f.events.length && f.events[next][0] === b) {
      const [, p, v] = f.events[next++];
      ex.smkaf_set_param(h, p, v);
    }
    const io = new Float32Array(ex.memory.buffer, ex.smkaf_buffer(h), block * ch);
    io.set(input.subarray(b * block * ch, (b + 1) * block * ch));
    ex.smkaf_process(h, block);
    output.set(new Float32Array(ex.memory.buffer, ex.smkaf_buffer(h), block * ch), b * block * ch);
    meter[b] = ex.smkaf_meter(h);
  }
  ex.smkaf_free(h);

  const a = new Uint32Array(output.buffer);
  const e = new Uint32Array(expected.buffer, expected.byteOffset, expected.length);
  let mismatch = a.findIndex((v, i) => v !== e[i]);
  const ma = new Uint32Array(meter.buffer);
  const me = new Uint32Array(expectedMeter.buffer, expectedMeter.byteOffset, expectedMeter.length);
  const meterMismatch = ma.findIndex((v, i) => v !== me[i]);

  if (mismatch < 0 && meterMismatch < 0) {
    console.log(`ok    ${f.name} (${output.length} samples, ${f.events.length} param changes)`);
  } else {
    failed++;
    if (mismatch >= 0) {
      const diff = output.reduce((m, v, i) => Math.max(m, Math.abs(v - expected[i])), 0);
      console.log(`FAIL  ${f.name}: first sample mismatch at ${mismatch} (frame ${Math.floor(mismatch / ch)}): wasm ${output[mismatch]} native ${expected[mismatch]}, max abs diff ${diff}`);
    }
    if (meterMismatch >= 0) {
      console.log(`FAIL  ${f.name}: meter mismatch at block ${meterMismatch}: wasm ${meter[meterMismatch]} native ${expectedMeter[meterMismatch]}`);
    }
  }
}
process.exit(failed ? 1 : 0);
