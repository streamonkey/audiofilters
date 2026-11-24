// AudioWorkletProcessor for all smkaf filters. One processor class is
// registered per filter as `smkaf-<name>`, with one k-rate AudioParam per
// filter parameter. The compiled WebAssembly.Module is passed in through
// processorOptions, because the worklet scope has no fetch().
//
// The build inlines the filter metadata for the placeholder below.
const FILTERS = __SMKAF_FILTERS__;

/** How many render quanta between meter messages to the main thread. */
const METER_INTERVAL = 4;

class SmkafProcessor extends AudioWorkletProcessor {
  constructor(options, kind) {
    super();
    const { module, channels } = options.processorOptions;
    this.params = FILTERS[kind].params;
    this.channels = channels;
    this.last = new Float32Array(this.params.length).fill(NaN);
    this.alive = true;
    this.handle = 0;
    this.fresh = true;
    this.blocks = 0;
    this.latency = -1;

    this.port.onmessage = (e) => {
      if (e.data.type === 'dispose') this.dispose();
      else if (e.data.type === 'reset' && this.handle) this.exports.smkaf_reset(this.handle);
    };

    WebAssembly.instantiate(module, {}).then(
      (instance) => {
        if (!this.alive) return;
        this.exports = instance.exports;
        this.maxFrames = this.exports.smkaf_max_frames();
        this.handle = this.exports.smkaf_new(kind, sampleRate, channels);
        if (!this.handle) throw new Error(`smkaf_new(${kind}, ${sampleRate}, ${channels}) failed`);
        this.port.postMessage({ type: 'ready' });
      },
    ).catch((err) => this.port.postMessage({ type: 'error', message: String(err) }));
  }

  /** Float32Array over the wasm I/O buffer; rebuilt when memory grows. */
  io() {
    const buffer = this.exports.memory.buffer;
    if (this.memory !== buffer) {
      this.memory = buffer;
      this.view = new Float32Array(buffer, this.exports.smkaf_buffer(this.handle), this.maxFrames * this.channels);
    }
    return this.view;
  }

  process(inputs, outputs, parameters) {
    if (!this.alive) return false;
    const input = inputs[0];
    const output = outputs[0];
    const ch = this.channels;

    if (!this.handle) {
      // Not instantiated yet: pass audio through untouched.
      for (let c = 0; c < output.length; c++) {
        if (input[c]) output[c].set(input[c]);
      }
      return true;
    }

    const ex = this.exports;
    for (let i = 0; i < this.params.length; i++) {
      const v = parameters[this.params[i].name][0];
      if (v !== this.last[i]) {
        ex.smkaf_set_param(this.handle, i, v);
        this.last[i] = v;
      }
    }
    if (this.fresh) {
      // Start at the initial parameter values instead of smoothing towards them.
      ex.smkaf_reset(this.handle);
      this.fresh = false;
    }

    const frames = output[0].length;
    for (let offset = 0; offset < frames; offset += this.maxFrames) {
      const n = Math.min(this.maxFrames, frames - offset);
      const io = this.io();
      for (let c = 0; c < ch; c++) {
        const src = input[c];
        for (let i = 0; i < n; i++) io[i * ch + c] = src ? src[offset + i] : 0;
      }
      ex.smkaf_process(this.handle, n);
      const out = this.io();
      for (let c = 0; c < ch; c++) {
        const dst = output[c];
        for (let i = 0; i < n; i++) dst[offset + i] = out[i * ch + c];
      }
    }

    const latency = ex.smkaf_latency(this.handle);
    if (latency !== this.latency) {
      this.latency = latency;
      this.port.postMessage({ type: 'latency', frames: latency });
    }
    if (++this.blocks % METER_INTERVAL === 0) {
      this.port.postMessage({ type: 'meter', value: ex.smkaf_meter(this.handle) });
    }
    return true;
  }

  dispose() {
    if (this.handle) this.exports.smkaf_free(this.handle);
    this.handle = 0;
    this.alive = false;
  }
}

FILTERS.forEach((filter, kind) => {
  registerProcessor(
    `smkaf-${filter.name}`,
    class extends SmkafProcessor {
      static get parameterDescriptors() {
        return filter.params.map((p) => ({
          name: p.name,
          defaultValue: p.default,
          minValue: p.min,
          maxValue: p.max,
          automationRate: 'k-rate',
        }));
      }

      constructor(options) {
        super(options, kind);
      }
    },
  );
});
