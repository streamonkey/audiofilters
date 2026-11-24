// Runs dist/worklet.js in Node with a minimal AudioWorkletGlobalScope stub
// and checks that it produces the same output as calling the wasm ABI
// directly (i.e. interleaving, parameter handling and buffering are right).
//
// usage: node tests/worklet/worklet.test.mjs <npm/dist>
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const dist = resolve(process.argv[2] ?? 'npm/dist');
const processors = new Map();
globalThis.sampleRate = 48000;
globalThis.AudioWorkletProcessor = class {
  constructor() {
    this.port = { onmessage: null, messages: [], postMessage(m) { this.messages.push(m); } };
  }
};
globalThis.registerProcessor = (name, cls) => processors.set(name, cls);
await import(pathToFileURL(join(dist, 'worklet.js')).href);

const filters = JSON.parse(readFileSync(join(dist, 'filters.json'), 'utf8'));
assert.equal(processors.size, filters.length);
const module = await WebAssembly.compile(readFileSync(join(dist, 'smkaf.wasm')));

const channels = 2;
const quanta = 200;
const signal = (c, i) => Math.sin(i * (0.01 + c * 0.003)) * (i % 9000 < 4500 ? 0.9 : 0.02);

for (const [kind, f] of filters.entries()) {
  const Proc = processors.get(`smkaf-${f.name}`);
  const descriptors = Proc.parameterDescriptors;
  assert.deepEqual(descriptors.map((d) => d.name), f.params.map((p) => p.name));

  const proc = new Proc({ processorOptions: { module, channels } });
  await new Promise((r) => setTimeout(r, 10)); // async instantiation
  assert.deepEqual(proc.port.messages.shift(), { type: 'ready' });

  // Reference: direct ABI calls with the same parameter changes.
  const { exports: ex } = await WebAssembly.instantiate(module, {});
  const h = ex.smkaf_new(kind, sampleRate, channels);
  const p0 = f.params[0];
  const changed = p0.min + (p0.max - p0.min) * 0.25;

  for (let q = 0; q < quanta; q++) {
    const params = Object.fromEntries(descriptors.map((d) => [d.name, new Float32Array([d.defaultValue])]));
    if (q >= 50) params[p0.name][0] = changed;
    if (q === 50) ex.smkaf_set_param(h, 0, Math.fround(changed));

    const input = [0, 1].map((c) => Float32Array.from({ length: 128 }, (_, i) => signal(c, q * 128 + i)));
    const output = [new Float32Array(128), new Float32Array(128)];
    assert.equal(proc.process([input], [output], params), true);

    const io = new Float32Array(ex.memory.buffer, ex.smkaf_buffer(h), 128 * channels);
    for (let i = 0; i < 128; i++) for (let c = 0; c < channels; c++) io[i * channels + c] = input[c][i];
    ex.smkaf_process(h, 128);
    const ref = new Float32Array(ex.memory.buffer, ex.smkaf_buffer(h), 128 * channels);
    for (let i = 0; i < 128; i++) {
      for (let c = 0; c < channels; c++) {
        assert.equal(output[c][i], ref[i * channels + c], `${f.name}: quantum ${q} frame ${i} ch ${c}`);
      }
    }
  }
  assert.ok(proc.port.messages.some((m) => m.type === 'meter'));
  proc.dispose();
  assert.equal(proc.process([[]], [[new Float32Array(128)]], {}), false);
  console.log(`ok    worklet ${f.name}`);
}
