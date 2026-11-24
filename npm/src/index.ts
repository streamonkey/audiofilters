import { FILTERS } from './generated.js';

export { FILTERS };

type AnyFilter = (typeof FILTERS)[number];
export type FilterName = AnyFilter['name'];
export type FilterMeta<N extends FilterName = FilterName> = Extract<AnyFilter, { name: N }>;
export type ParamMeta<N extends FilterName = FilterName> = FilterMeta<N>['params'][number];
export type ParamName<N extends FilterName> = ParamMeta<N>['name'];
/** Widened parameter metadata, as found in `filters.json`. */
export interface ParamInfo {
  name: string;
  label: string;
  unit: string;
  kind: 'float' | 'bool' | 'choice';
  min: number;
  max: number;
  default: number;
  options?: readonly string[];
}

/** Floats are numbers, booleans may be given as boolean, choices as option name or index. */
export type ParamValue = number | boolean | string;

interface Exports {
  memory: WebAssembly.Memory;
  smkaf_new(kind: number, sampleRate: number, channels: number): number;
  smkaf_free(handle: number): void;
  smkaf_set_param(handle: number, index: number, value: number): void;
  smkaf_graph(handle: number, x: number): number;
}

export interface LoadOptions {
  /** URL of smkaf.wasm. Defaults to the file shipped next to this module. */
  wasmUrl?: string | URL;
  /** URL of worklet.js. Defaults to the file shipped next to this module. */
  workletUrl?: string | URL;
}

export interface CreateOptions<N extends FilterName> {
  /** Number of channels processed (default 2). Inputs are up/down-mixed to it. */
  channels?: number;
  /** Initial parameter values. */
  params?: Partial<Record<ParamName<N>, ParamValue>>;
}

const worklets = new WeakMap<BaseAudioContext, Promise<void>>();

async function compile(url: string | URL): Promise<WebAssembly.Module> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`failed to load ${url}: ${response.status}`);
  if (response.headers.get('content-type')?.startsWith('application/wasm')) {
    return WebAssembly.compileStreaming(response);
  }
  return WebAssembly.compile(await response.arrayBuffer());
}

/**
 * Entry point: loads the wasm module and registers the worklet processors
 * on an AudioContext, then creates filter nodes.
 *
 * ```ts
 * const filters = await AudioFilters.load(ctx);
 * const comp = filters.create('compressor', { params: { threshold: -24 } });
 * source.connect(comp.node).connect(ctx.destination);
 * ```
 */
export class AudioFilters {
  private constructor(
    readonly context: BaseAudioContext,
    readonly module: WebAssembly.Module,
    private readonly mirror: Exports,
  ) {}

  static async load(context: BaseAudioContext, options: LoadOptions = {}): Promise<AudioFilters> {
    const wasmUrl = options.wasmUrl ?? new URL('./smkaf.wasm', import.meta.url);
    const workletUrl = options.workletUrl ?? new URL('./worklet.js', import.meta.url);

    let registered = worklets.get(context);
    if (!registered) {
      registered = context.audioWorklet.addModule(workletUrl);
      worklets.set(context, registered);
      registered.catch(() => worklets.delete(context));
    }
    const [module] = await Promise.all([compile(wasmUrl), registered]);
    // A main-thread instance mirrors every node's parameters so graphs can
    // be computed synchronously with the same code as the audio path.
    const instance = await WebAssembly.instantiate(module, {});
    return new AudioFilters(context, module, instance.exports as unknown as Exports);
  }

  create<N extends FilterName>(name: N, options: CreateOptions<N> = {}): FilterNode<N> {
    const kind = FILTERS.findIndex((f) => f.name === name);
    if (kind < 0) throw new Error(`unknown filter ${name}`);
    return new FilterNode<N>(this.context, this.module, this.mirror, kind, options);
  }
}

export class FilterNode<N extends FilterName> {
  readonly node: AudioWorkletNode;
  readonly meta: FilterMeta<N>;
  readonly channels: number;
  /** Latest meter value (see `meta.meter`), updated about every 10 ms. */
  meter = 0;
  /** Processing latency in frames, as reported by the worklet. */
  latency = 0;
  onmeter?: (value: number) => void;
  /** Resolves once the worklet has instantiated the filter (until then audio passes through). */
  readonly ready: Promise<void>;

  private readonly params: readonly ParamInfo[];
  private readonly values: Float32Array;
  private mirrorHandle: number;

  /** @internal use {@link AudioFilters.create} */
  constructor(
    context: BaseAudioContext,
    module: WebAssembly.Module,
    private readonly mirror: Exports,
    kind: number,
    options: CreateOptions<N>,
  ) {
    this.meta = FILTERS[kind] as unknown as FilterMeta<N>;
    this.params = FILTERS[kind].params as readonly ParamInfo[];
    this.channels = options.channels ?? 2;
    this.values = Float32Array.from(this.params, (p) => p.default);

    const parameterData: Record<string, number> = {};
    for (const [name, value] of Object.entries(options.params ?? {})) {
      const i = this.index(name);
      this.values[i] = this.toNumber(i, value as ParamValue);
      parameterData[name] = this.values[i];
    }

    this.node = new AudioWorkletNode(context, `smkaf-${this.meta.name}`, {
      numberOfInputs: 1,
      numberOfOutputs: 1,
      channelCount: this.channels,
      channelCountMode: 'explicit',
      channelInterpretation: 'speakers',
      outputChannelCount: [this.channels],
      parameterData,
      processorOptions: { module, channels: this.channels },
    });

    this.ready = new Promise((resolve, reject) => {
      this.node.port.onmessage = (e: MessageEvent) => {
        const msg = e.data;
        if (msg.type === 'meter') {
          this.meter = msg.value;
          this.onmeter?.(msg.value);
        } else if (msg.type === 'latency') {
          this.latency = msg.frames;
        } else if (msg.type === 'ready') {
          resolve();
        } else if (msg.type === 'error') {
          reject(new Error(msg.message));
        }
      };
    });

    this.mirrorHandle = mirror.smkaf_new(kind, context.sampleRate, this.channels);
    this.values.forEach((v, i) => mirror.smkaf_set_param(this.mirrorHandle, i, v));
  }

  /** The AudioParam of a parameter, for automation. */
  param(name: ParamName<N>): AudioParam {
    const p = this.node.parameters.get(name);
    if (!p) throw new Error(`unknown parameter ${name}`);
    return p;
  }

  set(name: ParamName<N>, value: ParamValue): void {
    const handle = this.handle();
    const i = this.index(name);
    const v = this.toNumber(i, value);
    this.values[i] = v;
    this.param(name).value = v;
    this.mirror.smkaf_set_param(handle, i, v);
  }

  /** Current value as number (booleans 0/1, choices as index). */
  get(name: ParamName<N>): number {
    return this.values[this.index(name)];
  }

  /** Current value of a choice parameter as option name. */
  getChoice(name: ParamName<N>): string | undefined {
    return this.params[this.index(name)].options?.[this.get(name)];
  }

  /**
   * Evaluate the filter's graph (`meta.graph`): for "transfer" `xs` are
   * input levels in dBFS, for "frequency" frequencies in Hz. Uses the values
   * set through {@link set}, not AudioParam automation.
   */
  graph(xs: ArrayLike<number>): Float32Array {
    const out = new Float32Array(xs.length);
    const handle = this.handle();
    for (let i = 0; i < xs.length; i++) out[i] = this.mirror.smkaf_graph(handle, xs[i]);
    return out;
  }

  /** Clear the filter's signal state (delay lines, envelopes). */
  reset(): void {
    this.node.port.postMessage({ type: 'reset' });
  }

  dispose(): void {
    this.node.disconnect();
    this.node.port.postMessage({ type: 'dispose' });
    if (this.mirrorHandle) this.mirror.smkaf_free(this.mirrorHandle);
    this.mirrorHandle = 0;
  }

  private handle(): number {
    if (!this.mirrorHandle) throw new Error(`${this.meta.name} node is disposed`);
    return this.mirrorHandle;
  }

  private index(name: string): number {
    const i = this.params.findIndex((p) => p.name === name);
    if (i < 0) throw new Error(`${this.meta.name} has no parameter ${name}`);
    return i;
  }

  /** Convert to the number the filter uses: clamped, and rounded for booleans and choices. */
  private toNumber(i: number, value: ParamValue): number {
    const p = this.params[i];
    if (typeof value === 'string') {
      const o = p.options?.indexOf(value) ?? -1;
      if (o < 0) throw new Error(`invalid value ${value} for ${p.name}`);
      return o;
    }
    const v = Math.min(p.max, Math.max(p.min, Number(value)));
    return p.kind === 'float' ? v : Math.round(v);
  }
}
