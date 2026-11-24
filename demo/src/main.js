import { AudioFilters, FILTERS } from '@streamonkey/audiofilters';

const $ = (id) => document.getElementById(id);
const dbToGain = (db) => 10 ** (db / 20);
const gainToDb = (g) => (g > 0 ? 20 * Math.log10(g) : -Infinity);

let ctx;
let filters;
let inputGain;
let chainOut;
let master;
let analysers = [];
let source = null;
let micStream = null;
let fileBuffer = null;
/** @type {{ node: import('@streamonkey/audiofilters').FilterNode<any>, bypass: boolean, el: HTMLElement, draw: () => void }[]} */
const chain = [];

for (const f of FILTERS) {
  $('filter-kind').append(new Option(f.label, f.name));
}

// ---------------------------------------------------------------- audio graph

$('start').onclick = async () => {
  if (ctx) {
    if (ctx.state === 'running') await ctx.suspend();
    else await ctx.resume();
    $('start').textContent = ctx.state === 'running' ? 'Pause' : 'Resume';
    return;
  }
  ctx = new AudioContext({ latencyHint: 'interactive' });
  filters = await AudioFilters.load(ctx);

  inputGain = ctx.createGain();
  inputGain.gain.value = dbToGain(Number($('input-gain').value));
  chainOut = ctx.createGain();
  master = ctx.createGain();
  master.gain.value = dbToGain(Number($('volume').value));
  chainOut.connect(master).connect(ctx.destination);
  const splitter = ctx.createChannelSplitter(2);
  master.connect(splitter);
  analysers = [0, 1].map((c) => {
    const a = ctx.createAnalyser();
    a.fftSize = 2048;
    splitter.connect(a, c);
    return a;
  });

  for (const id of ['source', 'add', 'preset', 'clear', 'bypass-all']) $(id).disabled = false;
  $('start').textContent = 'Pause';
  await setSource($('source').value);
  loadPreset();
  requestAnimationFrame(drawOutputMeter);
};

function rewire() {
  inputGain.disconnect();
  for (const c of chain) c.node.node.disconnect();
  let prev = inputGain;
  if (!$('bypass-all').checked) {
    for (const c of chain) {
      if (c.bypass) continue;
      prev.connect(c.node.node);
      prev = c.node.node;
    }
  }
  prev.connect(chainOut);
}

$('bypass-all').onchange = rewire;

$('input-gain').oninput = (e) => {
  const db = Number(e.target.value);
  $('input-gain-value').textContent = `${db} dB`;
  inputGain?.gain.setTargetAtTime(dbToGain(db), ctx.currentTime, 0.01);
};

$('volume').oninput = (e) => {
  const db = Number(e.target.value);
  $('volume-value').textContent = `${db} dB`;
  master?.gain.setTargetAtTime(dbToGain(db), ctx.currentTime, 0.01);
};

// -------------------------------------------------------------------- sources

/** Pink noise (Paul Kellet's filter) with a loud / quiet / silent envelope. */
function noiseBuffer() {
  const len = ctx.sampleRate * 8;
  const buf = ctx.createBuffer(2, len, ctx.sampleRate);
  for (let c = 0; c < 2; c++) {
    const d = buf.getChannelData(c);
    let b0 = 0, b1 = 0, b2 = 0, b3 = 0, b4 = 0, b5 = 0, b6 = 0;
    for (let i = 0; i < len; i++) {
      const w = Math.random() * 2 - 1;
      b0 = 0.99886 * b0 + w * 0.0555179;
      b1 = 0.99332 * b1 + w * 0.0750759;
      b2 = 0.969 * b2 + w * 0.153852;
      b3 = 0.8665 * b3 + w * 0.3104856;
      b4 = 0.55 * b4 + w * 0.5329522;
      b5 = -0.7616 * b5 - w * 0.016898;
      const pink = (b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362) * 0.11;
      b6 = w * 0.115926;
      d[i] = pink * envelope(i / ctx.sampleRate);
    }
  }
  return buf;
}

function toneBuffer() {
  const len = ctx.sampleRate * 8;
  const buf = ctx.createBuffer(2, len, ctx.sampleRate);
  for (let c = 0; c < 2; c++) {
    const d = buf.getChannelData(c);
    for (let i = 0; i < len; i++) {
      const t = i / ctx.sampleRate;
      const ess = t % 1 > 0.7 ? 0.3 * Math.sin(2 * Math.PI * 5000 * t) : 0;
      d[i] = (0.5 * Math.sin(2 * Math.PI * 220 * t) + ess) * envelope(t);
    }
  }
  return buf;
}

/** 3 s loud, 3 s at -30 dB, 2 s silence. */
function envelope(t) {
  if (t < 3) return 0.8;
  if (t < 6) return 0.8 * dbToGain(-30);
  return 0;
}

function playBuffer(buf) {
  const s = ctx.createBufferSource();
  s.buffer = buf;
  s.loop = true;
  s.connect(inputGain);
  s.start();
  return s;
}

async function setSource(kind) {
  if (source) {
    source.stop?.();
    source.disconnect();
    source = null;
  }
  micStream?.getTracks().forEach((t) => t.stop());
  micStream = null;

  if (kind === 'noise') source = playBuffer(noiseBuffer());
  else if (kind === 'tone') source = playBuffer(toneBuffer());
  else if (kind === 'file') {
    if (fileBuffer) source = playBuffer(fileBuffer);
    else $('file').click();
  } else if (kind === 'mic') {
    micStream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false },
    });
    source = ctx.createMediaStreamSource(micStream);
    source.connect(inputGain);
  }
}

$('source').onchange = (e) => {
  if (e.target.value === 'file') fileBuffer = null;
  setSource(e.target.value);
};

$('file').onchange = async (e) => {
  const file = e.target.files[0];
  if (!file) return;
  fileBuffer = await ctx.decodeAudioData(await file.arrayBuffer());
  setSource('file');
};

// ------------------------------------------------------------------ the chain

function loadPreset() {
  clearChain();
  addFilter('eq', { 'hp-freq': 80, 'band3-gain': 1.5 });
  addFilter('leveler');
  addFilter('compressor', { threshold: -24, ratio: 3, attack: 5, release: 120, makeup: 4 });
  addFilter('deesser');
  addFilter('limiter', { ceiling: -1, 'input-gain': 2 });
}

function clearChain() {
  for (const c of chain) {
    c.node.dispose();
    c.el.remove();
  }
  chain.length = 0;
  rewire();
}

$('add').onclick = () => addFilter($('filter-kind').value);
$('preset').onclick = loadPreset;
$('clear').onclick = clearChain;

function formatValue(p, v) {
  if (p.kind === 'bool') return v ? 'on' : 'off';
  if (p.kind === 'choice') return p.options[v];
  const digits = Math.abs(v) >= 100 ? 0 : Math.abs(v) >= 10 ? 1 : 2;
  return `${v.toFixed(digits)} ${p.unit}`.trim();
}

function control(entry, p) {
  const row = document.createElement('label');
  row.className = 'param';
  row.append(Object.assign(document.createElement('span'), { textContent: p.label }));
  const out = document.createElement('output');
  const node = entry.node;
  const current = node.get(p.name);

  let input;
  if (p.kind === 'float') {
    // Frequencies get a logarithmic slider.
    const log = p.unit === 'Hz';
    const toSlider = (v) => (log ? Math.log(v / p.min) / Math.log(p.max / p.min) : (v - p.min) / (p.max - p.min)) * 1000;
    const fromSlider = (s) => (log ? p.min * (p.max / p.min) ** (s / 1000) : p.min + (p.max - p.min) * (s / 1000));
    input = Object.assign(document.createElement('input'), { type: 'range', min: 0, max: 1000, step: 1 });
    input.value = toSlider(current);
    input.oninput = () => {
      const v = fromSlider(Number(input.value));
      node.set(p.name, v);
      out.textContent = formatValue(p, v);
      entry.draw();
    };
    input.ondblclick = () => {
      input.value = toSlider(p.default);
      input.oninput();
    };
    input.title = 'double-click to reset';
  } else if (p.kind === 'bool') {
    input = Object.assign(document.createElement('input'), { type: 'checkbox', checked: current !== 0 });
    input.onchange = () => {
      node.set(p.name, input.checked);
      out.textContent = formatValue(p, input.checked ? 1 : 0);
      entry.draw();
    };
  } else {
    input = document.createElement('select');
    p.options.forEach((o, i) => input.append(new Option(o, i)));
    input.value = current;
    input.onchange = () => {
      node.set(p.name, Number(input.value));
      out.textContent = formatValue(p, Number(input.value));
      entry.draw();
    };
  }
  out.textContent = formatValue(p, current);
  row.append(input, out);
  return row;
}

function addFilter(name, params = {}) {
  const node = filters.create(name, { params });
  const meta = node.meta;
  const el = document.createElement('article');
  el.className = 'filter';
  const entry = { node, bypass: false, el, draw: () => {} };

  const header = document.createElement('header');
  header.innerHTML = `<h2>${meta.label}</h2><span class="desc">${meta.description}</span>`;
  const bypass = Object.assign(document.createElement('input'), { type: 'checkbox' });
  bypass.onchange = () => {
    entry.bypass = bypass.checked;
    el.classList.toggle('bypassed', bypass.checked);
    rewire();
  };
  const bypassLabel = document.createElement('label');
  bypassLabel.className = 'toggle';
  bypassLabel.append(bypass, ' bypass');
  const button = (text, title, fn) => Object.assign(document.createElement('button'), { textContent: text, title, onclick: fn });
  header.append(
    bypassLabel,
    button('↑', 'move up', () => move(entry, -1)),
    button('↓', 'move down', () => move(entry, 1)),
    button('✕', 'remove', () => remove(entry)),
  );
  el.append(header);

  const body = document.createElement('div');
  body.className = 'body';
  const controls = document.createElement('div');
  controls.className = `params ${meta.name}`;
  for (const p of meta.params) controls.append(control(entry, p));
  body.append(controls);

  const side = document.createElement('div');
  side.className = 'side';
  if (meta.graph) {
    const canvas = document.createElement('canvas');
    side.append(canvas);
    entry.draw = () => drawGraph(canvas, node, meta.graph);
  }
  if (meta.meter) {
    const m = document.createElement('div');
    m.className = 'filter-meter';
    m.innerHTML = `<span>${meta.meter.name}</span><div class="bar"><div></div></div><output></output>`;
    const bar = m.querySelector('.bar > div');
    const text = m.querySelector('output');
    const bipolar = meta.meter.name !== 'gain-reduction';
    const range = meta.meter.unit === 'dB' ? 24 : 1;
    node.onmeter = (v) => {
      const x = Math.max(-1, Math.min(1, v / range));
      if (bipolar) {
        bar.style.left = `${50 + Math.min(0, x) * 50}%`;
        bar.style.width = `${Math.abs(x) * 50}%`;
      } else {
        bar.style.left = '0';
        bar.style.width = `${-x * 100}%`;
      }
      text.textContent = `${v.toFixed(meta.meter.unit === 'dB' ? 1 : 2)} ${meta.meter.unit}`;
    };
    side.append(m);
  }
  body.append(side);
  el.append(body);

  $('chain').append(el);
  chain.push(entry);
  rewire();
  requestAnimationFrame(entry.draw);
}

function move(entry, delta) {
  const i = chain.indexOf(entry);
  const j = i + delta;
  if (j < 0 || j >= chain.length) return;
  [chain[i], chain[j]] = [chain[j], chain[i]];
  for (const c of chain) $('chain').append(c.el);
  rewire();
}

function remove(entry) {
  chain.splice(chain.indexOf(entry), 1);
  entry.node.dispose();
  entry.el.remove();
  rewire();
}

// --------------------------------------------------------------------- graphs

function drawGraph(canvas, node, kind) {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth;
  const h = canvas.clientHeight;
  canvas.width = w * dpr;
  canvas.height = h * dpr;
  const g = canvas.getContext('2d');
  g.scale(dpr, dpr);
  const css = getComputedStyle(document.documentElement);
  const grid = css.getPropertyValue('--grid');
  const accent = css.getPropertyValue('--accent');
  const muted = css.getPropertyValue('--muted');
  g.font = '10px system-ui, sans-serif';
  g.fillStyle = muted;

  let xs, toX, toY, yLines, xLines;
  if (kind === 'transfer') {
    const lo = -60, hi = 6;
    toX = (db) => ((db - lo) / (hi - lo)) * w;
    toY = (db) => h - ((db - lo) / (hi - lo)) * h;
    xs = Array.from({ length: 200 }, (_, i) => lo + ((hi - lo) * i) / 199);
    xLines = yLines = [-48, -36, -24, -12, 0];
    g.strokeStyle = grid;
    g.setLineDash([3, 3]);
    g.beginPath();
    g.moveTo(toX(lo), toY(lo));
    g.lineTo(toX(hi), toY(hi));
    g.stroke();
    g.setLineDash([]);
  } else {
    const lo = Math.log10(20), hi = Math.log10(20000);
    toX = (f) => ((Math.log10(f) - lo) / (hi - lo)) * w;
    toY = (db) => h / 2 - (db / 24) * (h / 2);
    xs = Array.from({ length: 300 }, (_, i) => 10 ** (lo + ((hi - lo) * i) / 299));
    xLines = [50, 100, 200, 500, 1000, 2000, 5000, 10000];
    yLines = [-18, -12, -6, 0, 6, 12, 18];
  }

  g.strokeStyle = grid;
  g.lineWidth = 1;
  for (const x of xLines) {
    g.beginPath();
    g.moveTo(toX(x), 0);
    g.lineTo(toX(x), h);
    g.stroke();
    g.fillText(kind === 'frequency' ? (x >= 1000 ? `${x / 1000}k` : x) : x, toX(x) + 2, h - 3);
  }
  for (const y of yLines) {
    g.beginPath();
    g.moveTo(0, toY(y));
    g.lineTo(w, toY(y));
    g.stroke();
    g.fillText(y, 2, toY(y) - 2);
  }

  const ys = node.graph(xs);
  g.strokeStyle = accent;
  g.lineWidth = 2;
  g.beginPath();
  xs.forEach((x, i) => (i ? g.lineTo(toX(x), toY(ys[i])) : g.moveTo(toX(x), toY(ys[i]))));
  g.stroke();
}

window.addEventListener('resize', () => chain.forEach((c) => c.draw()));

// -------------------------------------------------------------- output meter

const peaks = [0, 0];
const scratch = new Float32Array(2048);
function drawOutputMeter() {
  analysers.forEach((a, c) => {
    a.getFloatTimeDomainData(scratch);
    let p = 0;
    for (const s of scratch) p = Math.max(p, Math.abs(s));
    peaks[c] = Math.max(p, peaks[c] * 0.92);
    const db = gainToDb(peaks[c]);
    const el = $(c ? 'meter-r' : 'meter-l');
    el.style.width = `${Math.max(0, Math.min(1, (db + 60) / 60)) * 100}%`;
    el.classList.toggle('hot', db > -1);
  });
  const db = gainToDb(Math.max(...peaks));
  $('peak-value').textContent = Number.isFinite(db) ? `${db.toFixed(1)} dBFS` : '-inf';
  requestAnimationFrame(drawOutputMeter);
}
