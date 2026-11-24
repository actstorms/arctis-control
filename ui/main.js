const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const $$ = (sel) => Array.from(document.querySelectorAll(sel));

const FREQS = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
const FREQ_LABELS = ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];

const EQ_PRESETS = [
  { key: "flat", name: "Flat", bands: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0] },
  { key: "bass", name: "Bass boost", bands: [6, 5, 4, 2, 0, 0, 0, 0, 0, 0] },
  { key: "footsteps", name: "Footsteps (FPS)", bands: [-4, -3, -2, -1, 0, 1.5, 4, 5, 3, 0] },
  { key: "immersive", name: "Immersive", bands: [4, 3, 1.5, 0, -1, 0, 1, 2, 3, 2.5] },
  { key: "music", name: "Music", bands: [4, 3, 1, 0, -1, -1, 0, 1.5, 3, 3] },
  { key: "voice", name: "Voice clarity", bands: [-3, -2, -1, 0, 1.5, 3, 3.5, 2.5, 1, 0] },
  { key: "treble", name: "Treble boost", bands: [0, 0, 0, 0, 0, 0, 1.5, 3.5, 5, 5] },
  { key: "bass-cut", name: "Bass cut", bands: [-6, -4.5, -3, -1, 0, 0, 0, 0, 0, 0] },
  { key: "loudness", name: "Low volume", bands: [5, 4, 2, 0, 0, 0, 0, 1, 3, 4] },
];

const MIC_PRESETS = {
  radio: ["Deep Radio", "Deep, dense FM-radio voice — rich low end, heavy leveling."],
  natural: ["Natural", "True to your voice, minus the room."],
  broadcast: ["Broadcast", "Warm, close voice with smooth, dense leveling."],
  podcast: ["Podcast", "Smooth and even for long conversations."],
  streaming: ["Streaming", "Bright and forward, cuts through game audio."],
  gaming: ["Gaming", "Maximum background removal for noisy rooms."],
};

const NC_TEXT = {
  2: "Active noise cancelling blocks out the room.",
  1: "Transparency lets you hear your surroundings.",
  0: "Noise control is off — saves a little battery.",
};

let settings = null;
let snapshot = null;
const micPresets = {};
let micStatus = null;
let cmStatus = null;
let currentPage = "overview";

// ------------------------------------------------------------ helpers

let toastTimer = null;
function toast(message, error = false) {
  const box = $("toast");
  box.textContent = message;
  box.classList.toggle("error", error);
  box.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => box.classList.remove("show"), error ? 5000 : 2800);
}

async function call(command, args) {
  try {
    return { ok: true, value: await invoke(command, args) };
  } catch (error) {
    toast(String(error), true);
    return { ok: false };
  }
}

/** Runs at most every `ms`, always delivering the latest arguments. */
function throttle(fn, ms) {
  let timer = null;
  let pending = null;
  const tick = () => {
    if (pending) {
      const args = pending;
      pending = null;
      fn(...args);
      timer = setTimeout(tick, ms);
    } else {
      timer = null;
    }
  };
  return (...args) => {
    pending = args;
    if (!timer) tick();
  };
}

function fill(input) {
  const min = Number(input.min);
  const max = Number(input.max);
  const p = ((Number(input.value) - min) / (max - min)) * 100;
  input.style.setProperty("--p", `${p}%`);
}

function setRange(id, value) {
  const input = $(id);
  if (!input || document.activeElement === input) return;
  input.value = String(value);
  fill(input);
}

const signed = (v, digits = 1) => `${v > 0 ? "+" : ""}${Number(v).toFixed(digits)}`;
const db = (v, digits = 1) => `${signed(v, digits)} dB`;

function duration(minutes) {
  if (minutes >= 60) return `${Math.floor(minutes / 60)} h ${String(minutes % 60).padStart(2, "0")} m`;
  return `${minutes} min`;
}

// --------------------------------------------------------- navigation

function showPage(page) {
  currentPage = page;
  $$(".nav").forEach((b) => b.classList.toggle("active", b.dataset.page === page));
  $$(".page").forEach((p) => p.classList.toggle("active", p.id === `page-${page}`));
  document.querySelector("main").scrollTop = 0;
  if (page === "eq") requestAnimationFrame(drawEq);
  if (page === "mic") refreshMic();
  if (page === "chatmix") refreshChatMix();
  updateMeters();
}

$$(".nav").forEach((b) => b.addEventListener("click", () => showPage(b.dataset.page)));
$$("[data-goto]").forEach((b) => b.addEventListener("click", () => showPage(b.dataset.goto)));

// ----------------------------------------------------------- overview

function setGauge(id, pct, cls) {
  const g = $(id);
  g.style.setProperty("--pct", pct ?? 0);
  g.className = `gauge ${cls || ""}`;
  g.querySelector(".pct").textContent = pct == null ? "--" : String(pct);
}

function stateWithBolt(id, text) {
  const el = $(id);
  el.innerHTML = '<svg class="bolt"><use href="#i-bolt"/></svg>';
  el.append(text);
}

function renderDevice() {
  const s = snapshot || {};
  const d = s.device;
  $("offline").hidden = Boolean(d);
  if (!d) {
    $("offline-reason").textContent = s.error ? `${s.error}.` : "Check the USB cable, then press refresh.";
  }

  // Sidebar
  $("side-status").className = "side-status" + (d ? (d.headset.online ? " online" : " idle") : "");
  $("side-title").textContent = !d ? "Not connected" : d.headset.online ? "Connected" : "Headset off";
  $("side-sub").textContent = !d
    ? "base station not found"
    : d.headset.online
      ? `Headset ${d.headset.percent}% · Spare ${d.spare.present ? d.spare.percent + "%" : "—"}`
      : "base station connected";

  // Headset battery
  const h = d?.headset;
  if (!h) {
    setGauge("gauge-headset", null, "dim");
    $("headset-state").textContent = "Not connected";
    $("headset-detail").textContent = "";
  } else if (!h.online) {
    setGauge("gauge-headset", null, "dim");
    $("headset-state").textContent = "Headset off";
    $("headset-detail").textContent = "Turn it on to see its battery.";
  } else {
    const low = h.percent <= (settings?.low_battery_percent ?? 25);
    setGauge("gauge-headset", h.percent, h.charging ? "charging" : low ? "low" : "");
    if (h.charging) {
      stateWithBolt("headset-state", "Charging");
      $("headset-detail").textContent = "Plugged in over USB.";
    } else {
      $("headset-state").textContent = "On battery";
      $("headset-detail").textContent =
        s.minutes_left != null
          ? `About ${duration(s.minutes_left)} left${s.estimate_learned ? "" : " (estimate)"}`
          : "";
    }
  }

  // Spare battery
  const sp = d?.spare;
  if (!sp) {
    setGauge("gauge-spare", null, "dim");
    $("spare-state").textContent = "—";
    $("spare-detail").textContent = "";
  } else if (!sp.present) {
    setGauge("gauge-spare", 0, "dim");
    $("spare-state").textContent = "Empty slot";
    $("spare-detail").textContent = "Insert the spare battery to charge it.";
  } else if (sp.charging) {
    setGauge("gauge-spare", sp.percent, "charging");
    stateWithBolt("spare-state", "Charging");
    $("spare-detail").textContent = `${sp.raw} of 8 bars · charging in the base station`;
  } else {
    setGauge("gauge-spare", sp.percent, "");
    $("spare-state").textContent = "Ready";
    $("spare-detail").textContent = "Fully charged — swap any time.";
  }

  const swap = h && h.online && !h.charging && h.percent <= 30 && sp && sp.percent >= 75;
  $("swap-hint").hidden = !swap;
  if (swap) {
    $("swap-text").textContent = `Headset is at ${h.percent}% and your spare is at ${sp.percent}%. Swap it into the headset.`;
  }

  // Noise control
  const mode = d ? d.noise_control : null;
  $$("#noise-control button").forEach((b) => {
    const on = Number(b.dataset.mode) === mode;
    b.classList.toggle("active", on);
    b.setAttribute("aria-checked", String(on));
    b.disabled = !d;
  });
  $("nc-desc").textContent = mode == null ? "Connect the headset to change this." : NC_TEXT[mode];

  // Values the device reports back
  if (d) {
    if (document.activeElement !== $("auto-off")) $("auto-off").value = String(d.auto_off);
    setRange("lights", d.lights);
    $("lights-val").textContent = String(d.lights);
  }

  // About
  $("about-node").textContent = s.node || "—";
  $("about-updated").textContent = s.updated ? new Date(s.updated * 1000).toLocaleTimeString() : "—";
  if (settings) {
    const hours = (settings.secs_per_step * 8) / 3600;
    $("about-estimate").textContent = `${hours.toFixed(1)} h per full battery (${s.estimate_learned ? "measured" : "rated, until measured"})`;
  }
}

$("noise-control").addEventListener("click", async (e) => {
  const b = e.target.closest("button[data-mode]");
  if (!b) return;
  const mode = Number(b.dataset.mode);
  $$("#noise-control button").forEach((x) => x.classList.toggle("active", x === b));
  $("nc-desc").textContent = NC_TEXT[mode];
  await call("set_noise_control", { mode });
});

$("refresh").addEventListener("click", () => {
  const btn = $("refresh");
  btn.classList.remove("spin");
  void btn.offsetWidth;
  btn.classList.add("spin");
  invoke("refresh");
  refreshMic();
  refreshChatMix();
});

function renderTiles() {
  if (!settings) return;
  const m = settings.mic;
  $("tile-mic-pill").textContent = m.enabled ? "On" : "Off";
  $("tile-mic-pill").classList.toggle("on", m.enabled);
  $("tile-mic").textContent = m.enabled
    ? `${MIC_PRESETS[m.preset]?.[0] || "Custom"} voice`
    : "Noise removal, compression & EQ";

  $("tile-eq-pill").textContent = settings.eq_bypass ? "Bypassed" : "On";
  $("tile-eq-pill").classList.toggle("on", !settings.eq_bypass);
  $("tile-eq").textContent = eqDisplayName();

  const c = settings.chatmix;
  $("tile-cm-pill").textContent = c.enabled ? "On" : "Off";
  $("tile-cm-pill").classList.toggle("on", c.enabled);
  const [g, ch] = levels(c.balance);
  $("tile-cm").textContent = c.enabled ? `Game ${g}% · Chat ${ch}%` : "Balance game and voice chat";
}

// ---------------------------------------------------------- equalizer

const canvas = $("eq-canvas");
const ctx = canvas.getContext("2d");
let dragBand = null;
let hoverBand = null;

function eqDisplayName() {
  if (!settings) return "";
  const n = settings.eq_name || "";
  if (n.startsWith("user:")) return n.slice(5);
  return EQ_PRESETS.find((p) => p.key === n)?.name || "Custom";
}

/** RBJ peaking filter magnitude in dB at `f` (approximates the headset's bands). */
function peakingDb(f, f0, gain, q = 1.41, fs = 48000) {
  if (gain === 0) return 0;
  const A = Math.pow(10, gain / 40);
  const w0 = (2 * Math.PI * f0) / fs;
  const alpha = Math.sin(w0) / (2 * q);
  const cos = Math.cos(w0);
  const b0 = 1 + alpha * A, b1 = -2 * cos, b2 = 1 - alpha * A;
  const a0 = 1 + alpha / A, a1 = -2 * cos, a2 = 1 - alpha / A;
  const w = (2 * Math.PI * f) / fs;
  const c1 = Math.cos(w), s1 = Math.sin(w), c2 = Math.cos(2 * w), s2 = Math.sin(2 * w);
  const nr = b0 + b1 * c1 + b2 * c2, ni = -(b1 * s1 + b2 * s2);
  const dr = a0 + a1 * c1 + a2 * c2, di = -(a1 * s1 + a2 * s2);
  return 10 * Math.log10((nr * nr + ni * ni) / (dr * dr + di * di));
}

function geometry() {
  const w = canvas.clientWidth;
  const h = canvas.clientHeight;
  const padX = 34;
  const padY = 26;
  const fx = (f) => padX + (Math.log(f / 20) / Math.log(1000)) * (w - padX * 2);
  const fy = (d) => h / 2 - (d / 12) * (h / 2 - padY);
  const yToDb = (y) => ((h / 2 - y) / (h / 2 - padY)) * 12;
  return { w, h, fx, fy, yToDb };
}

const cssVar = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();

function drawEq() {
  if (!settings || currentPage !== "eq") return;
  const dpr = window.devicePixelRatio || 1;
  const { w, h, fx, fy } = geometry();
  if (!w || !h) return;
  if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
  }
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const line = cssVar("--line-2");
  const muted = cssVar("--muted");
  const accent = cssVar("--accent");
  const text = cssVar("--text");

  ctx.font = "11px system-ui, sans-serif";
  ctx.lineWidth = 1;
  for (const d of [-12, -6, 0, 6, 12]) {
    ctx.strokeStyle = line;
    ctx.globalAlpha = d === 0 ? 0.9 : 0.4;
    ctx.beginPath();
    ctx.moveTo(30, Math.round(fy(d)) + 0.5);
    ctx.lineTo(w - 10, Math.round(fy(d)) + 0.5);
    ctx.stroke();
    ctx.globalAlpha = 1;
    ctx.fillStyle = muted;
    ctx.textAlign = "right";
    if (Math.abs(d) !== 12) ctx.fillText(d > 0 ? `+${d}` : String(d), 26, fy(d) + 4);
  }
  ctx.textAlign = "center";
  for (const [f, label] of [[100, "100 Hz"], [1000, "1 kHz"], [10000, "10 kHz"]]) {
    ctx.strokeStyle = line;
    ctx.globalAlpha = 0.4;
    ctx.beginPath();
    ctx.moveTo(Math.round(fx(f)) + 0.5, 16);
    ctx.lineTo(Math.round(fx(f)) + 0.5, h - 18);
    ctx.stroke();
    ctx.globalAlpha = 1;
    ctx.fillStyle = muted;
    ctx.fillText(label, fx(f), h - 5);
  }

  // Combined response
  const bands = settings.bands;
  const pts = [];
  for (let i = 0; i <= 240; i++) {
    const f = 20 * Math.pow(1000, i / 240);
    let sum = 0;
    for (let b = 0; b < 10; b++) sum += peakingDb(f, FREQS[b], bands[b]);
    pts.push([fx(f), fy(Math.max(-14, Math.min(14, sum)))]);
  }
  const grad = ctx.createLinearGradient(0, 0, 0, h);
  grad.addColorStop(0, accent + "55");
  grad.addColorStop(0.5, accent + "08");
  grad.addColorStop(1, accent + "55");
  ctx.beginPath();
  ctx.moveTo(pts[0][0], fy(0));
  pts.forEach(([x, y]) => ctx.lineTo(x, y));
  ctx.lineTo(pts[pts.length - 1][0], fy(0));
  ctx.closePath();
  ctx.fillStyle = grad;
  ctx.fill();

  ctx.beginPath();
  pts.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
  ctx.strokeStyle = accent;
  ctx.lineWidth = 2.5;
  ctx.stroke();

  // Handles
  bands.forEach((g, i) => {
    const x = fx(FREQS[i]);
    const y = fy(g);
    const active = i === dragBand || i === hoverBand;
    ctx.beginPath();
    ctx.arc(x, y, active ? 8 : 6, 0, Math.PI * 2);
    ctx.fillStyle = active ? accent : cssVar("--panel");
    ctx.fill();
    ctx.lineWidth = 2.5;
    ctx.strokeStyle = accent;
    ctx.stroke();
  });
  const label = dragBand ?? hoverBand;
  if (label != null) {
    const x = fx(FREQS[label]);
    const y = fy(bands[label]);
    ctx.fillStyle = text;
    ctx.textAlign = "center";
    ctx.font = "600 12px system-ui, sans-serif";
    ctx.fillText(`${FREQ_LABELS[label]} Hz  ${signed(bands[label])} dB`, Math.min(Math.max(x, 70), w - 70), y < 50 ? y + 26 : y - 14);
  }
}

function nearestBand(x) {
  const { fx } = geometry();
  let best = 0;
  let dist = Infinity;
  FREQS.forEach((f, i) => {
    const d = Math.abs(fx(f) - x);
    if (d < dist) {
      dist = d;
      best = i;
    }
  });
  return best;
}

const sendEq = throttle(() => call("set_equalizer", { bands: settings.bands, name: settings.eq_name }), 160);

function setBand(i, value) {
  const v = Math.max(-10, Math.min(10, Math.round(value * 2) / 2));
  if (settings.bands[i] === v) return;
  settings.bands[i] = v;
  settings.eq_name = "";
  renderBands();
  drawEq();
  renderEqPresets();
  renderTiles();
  sendEq();
}

canvas.addEventListener("pointerdown", (e) => {
  if (!settings) return;
  const r = canvas.getBoundingClientRect();
  dragBand = nearestBand(e.clientX - r.left);
  canvas.setPointerCapture(e.pointerId);
  setBand(dragBand, geometry().yToDb(e.clientY - r.top));
  drawEq();
});
canvas.addEventListener("pointermove", (e) => {
  if (!settings) return;
  const r = canvas.getBoundingClientRect();
  if (dragBand != null) {
    setBand(dragBand, geometry().yToDb(e.clientY - r.top));
  } else {
    const b = nearestBand(e.clientX - r.left);
    if (b !== hoverBand) {
      hoverBand = b;
      drawEq();
    }
  }
});
canvas.addEventListener("pointerup", () => {
  dragBand = null;
  drawEq();
});
canvas.addEventListener("pointerleave", () => {
  hoverBand = null;
  drawEq();
});
canvas.addEventListener("dblclick", (e) => {
  const r = canvas.getBoundingClientRect();
  setBand(nearestBand(e.clientX - r.left), 0);
});
canvas.addEventListener(
  "wheel",
  (e) => {
    if (!settings) return;
    e.preventDefault();
    const r = canvas.getBoundingClientRect();
    const b = nearestBand(e.clientX - r.left);
    hoverBand = b;
    setBand(b, settings.bands[b] + (e.deltaY < 0 ? 0.5 : -0.5));
    drawEq();
  },
  { passive: false },
);
new ResizeObserver(() => drawEq()).observe(canvas);

function buildBands() {
  const wrap = $("bands");
  FREQS.forEach((_, i) => {
    const row = document.createElement("div");
    row.className = "band";
    row.innerHTML = `<span class="db" id="db-${i}">0.0</span>
      <input type="range" min="-10" max="10" step="0.5" value="0" data-band="${i}" aria-label="${FREQ_LABELS[i]} Hz" />
      <span class="hz">${FREQ_LABELS[i]}</span>`;
    wrap.appendChild(row);
  });
  wrap.addEventListener("input", (e) => {
    const s = e.target.closest("input[data-band]");
    if (s) setBand(Number(s.dataset.band), Number(s.value));
  });
  wrap.addEventListener("dblclick", (e) => {
    const s = e.target.closest("input[data-band]");
    if (s) setBand(Number(s.dataset.band), 0);
  });
}

function renderBands() {
  settings.bands.forEach((v, i) => {
    const s = document.querySelector(`input[data-band="${i}"]`);
    if (s && document.activeElement !== s) s.value = String(v);
    if (s) fill(s);
    $(`db-${i}`).textContent = signed(v);
  });
  $("eq-name").textContent = eqDisplayName();
  $("eq-bypass").checked = settings.eq_bypass;
  document.querySelector(".eq-card").classList.toggle("bypassed", settings.eq_bypass);
}

function chip(label, active, onClick, onDelete) {
  const b = document.createElement("button");
  b.className = "chip" + (active ? " active" : "");
  b.append(label);
  b.addEventListener("click", onClick);
  if (onDelete) {
    const x = document.createElement("span");
    x.className = "del";
    x.title = "Delete preset";
    x.innerHTML = '<svg><use href="#i-x"/></svg>';
    x.addEventListener("click", (e) => {
      e.stopPropagation();
      onDelete();
    });
    b.append(x);
  }
  return b;
}

function applyEqPreset(name, bands) {
  settings.bands = bands.slice();
  settings.eq_name = name;
  renderBands();
  renderEqPresets();
  renderTiles();
  drawEq();
  call("set_equalizer", { bands: settings.bands, name });
}

function renderEqPresets() {
  $("eq-presets").replaceChildren(
    ...EQ_PRESETS.map((p) => chip(p.name, settings.eq_name === p.key, () => applyEqPreset(p.key, p.bands))),
  );
  const user = settings.eq_user_presets || [];
  $("eq-user-title").hidden = user.length === 0;
  $("eq-user-presets").replaceChildren(
    ...user.map((p) =>
      chip(
        p.name,
        settings.eq_name === `user:${p.name}`,
        () => applyEqPreset(`user:${p.name}`, p.bands),
        async () => {
          const r = await call("delete_eq_preset", { name: p.name });
          if (r.ok) {
            settings.eq_user_presets = r.value;
            if (settings.eq_name === `user:${p.name}`) settings.eq_name = "";
            renderEqPresets();
            renderBands();
            renderTiles();
          }
        },
      ),
    ),
  );
}

$("eq-save-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const name = $("eq-save-name").value.trim();
  if (!name) {
    $("eq-save-name").focus();
    return;
  }
  const r = await call("save_eq_preset", { name, bands: settings.bands });
  if (r.ok) {
    settings.eq_user_presets = r.value;
    settings.eq_name = `user:${name}`;
    $("eq-save-name").value = "";
    renderEqPresets();
    renderBands();
    renderTiles();
    toast(`Saved “${name}”.`);
  }
});

$("eq-bypass").addEventListener("change", async (e) => {
  const bypass = e.target.checked;
  const r = await call("set_eq_bypass", { bypass });
  if (r.ok) settings.eq_bypass = bypass;
  else e.target.checked = !bypass;
  renderBands();
  renderTiles();
});

// --------------------------------------------------------- microphone

/** One "Leveling" knob drives threshold, ratio and make-up together. */
function levelingToComp(amount) {
  const a = amount / 100;
  const ratio = 1 + a * 5;
  const threshold = -12 - a * 20;
  const makeup = Math.round(-threshold * (1 - 1 / ratio) * 0.55 * 2) / 2;
  return {
    comp_ratio: Math.round(ratio * 10) / 10,
    comp_threshold_db: Math.round(threshold),
    comp_makeup_db: makeup,
  };
}
const compToLeveling = (m) => Math.max(0, Math.min(100, Math.round(((m.comp_ratio - 1) / 5) * 100)));

const MIC_FIELDS = [
  ["mic-ns-strength", "ns_strength", (v) => `${v}%`],
  ["mic-gate-threshold", "gate_threshold_db", (v) => `${v} dB`],
  ["mic-breath", "breath_db", (v) => (Number(v) === 0 ? "Off" : `−${v} dB`)],
  ["mic-out", "output_db", (v) => db(v)],
  ["mic-warmth", "warmth_db", (v) => db(v)],
  ["mic-depth", "depth_db", (v) => db(v)],
  ["mic-leveler", "leveler", (v) => (Number(v) === 0 ? "Off" : `${v}%`)],
  ["mic-tube", "tube", (v) => (Number(v) === 0 ? "Off" : `${v}%`)],
  ["mic-body", "body", (v) => (Number(v) === 0 ? "Off" : `${v}%`)],
  ["mic-mud", "mud_db", (v) => db(v)],
  ["mic-clarity", "clarity_db", (v) => db(v)],
  ["mic-air", "air_db", (v) => db(v)],
  ["mic-hp", "highpass_hz", (v) => `${v} Hz`],
  ["mic-cthr", "comp_threshold_db", (v) => `${v} dB`],
  ["mic-cratio", "comp_ratio", (v) => `${Number(v).toFixed(1)}:1`],
  ["mic-catt", "comp_attack_ms", (v) => `${v} ms`],
  ["mic-crel", "comp_release_ms", (v) => `${v} ms`],
  ["mic-cmk", "comp_makeup_db", (v) => db(v)],
  ["mic-deess", "deess_db", (v) => (Number(v) === 0 ? "Off" : `−${Number(v).toFixed(1)} dB`)],
  ["mic-deess-hz", "deess_hz", (v) => `${(v / 1000).toFixed(1)} kHz`],
  ["mic-ceil", "ceiling_db", (v) => `${Number(v).toFixed(1)} dB`],
];

const pushMic = throttle(() => call("mic_update", { settings: settings.mic }), 120);

function micChanged() {
  settings.mic.preset = "custom";
  renderMic();
  renderTiles();
  pushMic();
}

let lastMicPreset = "radio";

function useMicPreset(key) {
  const p = micPresets[key];
  if (!p) return;
  settings.mic = { ...p, enabled: settings.mic.enabled, hw_volume: settings.mic.hw_volume };
  renderMic();
  renderTiles();
  pushMic();
}

function buildMic() {
  const grid = $("mic-presets");
  for (const [key, [name, desc]] of Object.entries(MIC_PRESETS)) {
    const b = document.createElement("button");
    b.className = "preset";
    b.dataset.preset = key;
    const strong = document.createElement("strong");
    strong.textContent = name;
    const span = document.createElement("span");
    span.textContent = desc;
    b.append(strong, span);
    b.addEventListener("click", () => useMicPreset(key));
    grid.appendChild(b);
  }

  for (const [id, key, fmt] of MIC_FIELDS) {
    $(id).addEventListener("input", (e) => {
      settings.mic[key] = Number(e.target.value);
      $(`${id}-val`).textContent = fmt(e.target.value);
      fill(e.target);
      micChanged();
    });
  }

  $("mic-comp-amount").addEventListener("input", (e) => {
    Object.assign(settings.mic, levelingToComp(Number(e.target.value)));
    fill(e.target);
    micChanged();
  });
  $("mic-ns").addEventListener("change", (e) => {
    settings.mic.noise_suppression = e.target.checked;
    micChanged();
  });
  $("mic-gate").addEventListener("change", (e) => {
    settings.mic.gate = e.target.checked;
    micChanged();
  });
  $("mic-reset").addEventListener("click", () => useMicPreset(lastMicPreset));
  $("mic-engine").addEventListener("click", async (e) => {
    const b = e.target.closest("button[data-engine]");
    if (!b || b.disabled || settings.mic.ns_engine === b.dataset.engine) return;
    settings.mic.ns_engine = b.dataset.engine;
    renderMic();
    // Switching engines rebuilds the chain (only the studio mic restarts).
    const r = await call("mic_update", { settings: settings.mic });
    if (r.ok) refreshMic();
  });

  const hwGain = throttle((level) => call("set_mic_volume", { level }), 150);
  for (const id of ["mic-hw", "hw-gain"]) {
    $(id).addEventListener("input", (e) => {
      settings.mic.hw_volume = Number(e.target.value);
      fill(e.target);
      renderMic();
      hwGain(settings.mic.hw_volume);
    });
  }

  $("mic-enabled").addEventListener("change", async (e) => {
    const on = e.target.checked;
    e.target.disabled = true;
    $("mic-switch-label").textContent = on ? "Starting…" : "Stopping…";
    const r = await call("mic_toggle", { on });
    e.target.disabled = false;
    if (r.ok) {
      settings.mic.enabled = on;
      micStatus = r.value;
      toast(on ? "Studio mic is on — pick “Arctis Mic (Studio)” in your apps." : "Studio mic is off.");
    } else {
      e.target.checked = !on;
    }
    renderMic();
    renderTiles();
    updateMeters(true);
  });

  $("mic-default").addEventListener("click", async () => {
    const r = await call("mic_set_default");
    if (r.ok) {
      toast("“Arctis Mic (Studio)” is now your default input.");
      refreshMic();
    }
  });

  // Test: record the studio and the raw mic together, then A/B them.
  $("mic-record").addEventListener("click", async () => {
    const btn = $("mic-record");
    if (btn.classList.contains("recording")) return;
    await call("mic_test_play", { which: "stop" });
    btn.classList.add("recording");
    let left = 6;
    const label = btn.querySelector("span");
    label.textContent = `Recording… ${left}`;
    const timer = setInterval(() => {
      left = Math.max(0, left - 1);
      label.textContent = `Recording… ${left}`;
    }, 1000);
    const r = await call("mic_test_record", { seconds: 6 });
    clearInterval(timer);
    btn.classList.remove("recording");
    label.textContent = "Record again";
    if (r.ok) {
      $("mic-play-raw").disabled = false;
      $("mic-play-studio").disabled = !r.value;
      if (r.value) {
        call("mic_test_play", { which: "studio" });
        toast("Playing the studio version — compare with “Raw”.");
      } else {
        toast("Recorded the raw mic. Turn the studio mic on to compare.");
      }
    }
  });
  $("mic-play-studio").addEventListener("click", () => call("mic_test_play", { which: "studio" }));
  $("mic-play-raw").addEventListener("click", () => call("mic_test_play", { which: "raw" }));

  $("mic-listen").addEventListener("change", async (e) => {
    const r = await call("mic_monitor", { on: e.target.checked });
    if (!r.ok) e.target.checked = false;
    else if (e.target.checked) toast("Listening to the studio mic. Use headphones to avoid feedback.");
  });
}

function renderMic() {
  if (!settings) return;
  const m = settings.mic;
  if (MIC_PRESETS[m.preset]) lastMicPreset = m.preset;

  $("mic-enabled").checked = m.enabled;
  if (!$("mic-enabled").disabled) $("mic-switch-label").textContent = m.enabled ? "On" : "Off";
  $("page-mic").classList.toggle("off", !m.enabled);
  $$("#mic-presets .preset").forEach((b) => b.classList.toggle("active", b.dataset.preset === m.preset));

  for (const [id, key, fmt] of MIC_FIELDS) {
    setRange(id, m[key]);
    $(`${id}-val`).textContent = fmt(m[key]);
  }
  const leveling = compToLeveling(m);
  setRange("mic-comp-amount", leveling);
  $("mic-comp-amount-val").textContent = `${leveling}%`;
  $("mic-ns").checked = m.noise_suppression;
  $("mic-gate").checked = m.gate;
  $("mic-ns-strength").disabled = !m.noise_suppression;
  const avail = { deepfilter: micStatus?.deepfilter ?? true, rnnoise: micStatus?.rnnoise ?? true };
  const running = micStatus?.engine;
  const shown = m.enabled && running && running !== "none" ? running : m.ns_engine;
  for (const b of document.querySelectorAll("#mic-engine button")) {
    b.disabled = !avail[b.dataset.engine];
    b.title = avail[b.dataset.engine] ? "" : "Not installed";
    b.classList.toggle("active", shown === b.dataset.engine);
  }
  $("mic-gate-threshold").disabled = !m.gate;
  for (const id of ["mic-hw", "hw-gain"]) setRange(id, m.hw_volume);
  $("mic-hw-val").textContent = `${m.hw_volume}/10`;
  $("hw-gain-val").textContent = `${m.hw_volume}/10`;

  const st = micStatus;
  const dot = $("mic-dot");
  $("mic-default").hidden = true;
  if (!m.enabled) {
    dot.className = "status-dot";
    $("mic-state").textContent = "Off — apps use the raw headset mic";
    $("mic-detail").textContent = "Turn on to add “Arctis Mic (Studio)” to your inputs.";
  } else if (st && !st.active) {
    dot.className = "status-dot idle";
    $("mic-state").textContent = "Starting…";
    $("mic-detail").textContent = "Waiting for the studio microphone to appear.";
  } else {
    dot.className = "status-dot on";
    $("mic-state").textContent = "On — “Arctis Mic (Studio)”";
    if (st?.is_default) {
      $("mic-detail").textContent = "It is your default input, so apps use it automatically.";
    } else {
      $("mic-detail").textContent = "Select it in your apps, or make it the default input.";
      $("mic-default").hidden = false;
    }
  }

  const warn = [];
  if (st && !st.hardware) warn.push("Headset microphone not detected.");
  if (st && !st.rnnoise && !st.deepfilter) warn.push("AI noise removal needs the deepfilternet-ladspa (or noise-suppression-for-voice) package.");
  if (st && !st.dsp) warn.push("Compressor plugin (libarctis_dsp.so) not found — run cargo build --release.");
  $("mic-warn").hidden = warn.length === 0;
  $("mic-warn").textContent = warn.join(" ");
}

async function refreshMic() {
  const r = await call("mic_status");
  if (r.ok) {
    micStatus = r.value;
    renderMic();
  }
}

// Level meters run only while the mic page is on screen.
let metersOn = false;
async function updateMeters(restart = false) {
  const want = currentPage === "mic" && !document.hidden;
  if (want === metersOn && !restart) return;
  metersOn = want;
  if (!want) resetMeters();
  await invoke("mic_meter", { on: want }).catch(() => {});
}
document.addEventListener("visibilitychange", () => updateMeters());

function resetMeters() {
  for (const which of ["raw", "studio"]) {
    const m = $(`meter-${which}`);
    m.querySelector(".rms").style.width = "0";
    m.querySelector(".peak").style.left = "0";
    m.classList.remove("hot", "clip");
    $(`meter-${which}-val`).textContent = "—";
  }
}

const meterPct = (d) => Math.max(0, Math.min(100, ((d + 60) / 60) * 100));
function onMeter({ which, peak, rms }) {
  if (!metersOn) return;
  const m = $(`meter-${which}`);
  if (!m) return;
  m.querySelector(".rms").style.width = `${meterPct(rms)}%`;
  m.querySelector(".peak").style.left = `calc(${meterPct(peak)}% - 2px)`;
  m.classList.toggle("hot", peak > -6);
  m.classList.toggle("clip", peak > -0.5);
  $(`meter-${which}-val`).textContent = peak <= -100 ? "silent" : `${peak.toFixed(0)} dB`;
}

// ------------------------------------------------------------ chatmix

function levels(balance) {
  return [balance <= 0 ? 100 : 100 - balance, balance >= 0 ? 100 : 100 + balance];
}

function renderChatMix() {
  if (!settings) return;
  const c = settings.chatmix;
  $("cm-enabled").checked = c.enabled;
  if (!$("cm-enabled").disabled) $("cm-switch-label").textContent = c.enabled ? "On" : "Off";
  $("page-chatmix").classList.toggle("disabled", !c.enabled);
  setRange("cm-balance", c.balance);
  const [g, ch] = levels(c.balance);
  $("cm-game").textContent = `${g}%`;
  $("cm-chat").textContent = `${ch}%`;
  $("cm-game-bar").style.height = `${g}%`;
  $("cm-chat-bar").style.height = `${ch}%`;
  $("cm-dial").checked = c.use_dial;

  const def = cmStatus?.default_sink;
  $$("#cm-default button").forEach((b) => b.classList.toggle("active", def === `arctis_${b.dataset.sink}`));
  $("cm-default-now").textContent = !c.enabled
    ? "Turn ChatMix on first."
    : def === "arctis_game"
      ? "New audio plays through Game."
      : def === "arctis_chat"
        ? "New audio plays through Chat."
        : "Your default output is another device.";
  for (const el of $$("#cm-default button, #cm-balance, #cm-dial, #cm-center")) el.disabled = !c.enabled;
}

async function refreshChatMix() {
  const r = await call("chatmix_status");
  if (r.ok) {
    cmStatus = r.value;
    renderChatMix();
  }
}

function buildChatMix() {
  const send = throttle((balance) => call("chatmix_set_balance", { balance }), 80);
  $("cm-balance").addEventListener("input", (e) => {
    let v = Number(e.target.value);
    if (Math.abs(v) <= 4) v = 0; // detent at the centre
    settings.chatmix.balance = v;
    fill(e.target);
    renderChatMix();
    renderTiles();
    send(v);
  });
  $("cm-center").addEventListener("click", () => {
    settings.chatmix.balance = 0;
    renderChatMix();
    renderTiles();
    send(0);
  });
  $("cm-enabled").addEventListener("change", async (e) => {
    const on = e.target.checked;
    e.target.disabled = true;
    $("cm-switch-label").textContent = on ? "Starting…" : "Stopping…";
    const r = await call("chatmix_toggle", { on });
    e.target.disabled = false;
    if (r.ok) {
      settings.chatmix.enabled = on;
      cmStatus = r.value;
      toast(on ? "ChatMix is on — send your voice app to “Arctis Chat”." : "ChatMix is off.");
    } else {
      e.target.checked = !on;
    }
    renderChatMix();
    renderTiles();
  });
  $("cm-dial").addEventListener("change", async (e) => {
    const on = e.target.checked;
    const r = await call("chatmix_set_dial", { on });
    if (r.ok) settings.chatmix.use_dial = on;
    else e.target.checked = !on;
  });
  $("cm-default").addEventListener("click", async (e) => {
    const b = e.target.closest("button[data-sink]");
    if (!b) return;
    const r = await call("chatmix_set_default", { which: b.dataset.sink });
    if (r.ok) refreshChatMix();
  });
}

// ---------------------------------------------------- headset/settings

function renderHeadset() {
  if (!settings) return;
  $$("#sidetone button").forEach((b) => b.classList.toggle("active", Number(b.dataset.level) === settings.sidetone));
  if (!snapshot?.device) {
    $("auto-off").value = String(settings.inactive);
    setRange("lights", settings.lights);
    $("lights-val").textContent = String(settings.lights);
  }
  setRange("low-battery", settings.low_battery_percent);
  $("low-battery-val").textContent = `${settings.low_battery_percent}%`;
  $("spare-notify").checked = settings.notify_spare_ready;
}

function buildHeadset() {
  $("sidetone").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-level]");
    if (!b) return;
    settings.sidetone = Number(b.dataset.level);
    renderHeadset();
    call("set_sidetone", { level: settings.sidetone });
  });
  $("auto-off").addEventListener("change", (e) => call("set_auto_off", { level: Number(e.target.value) }));
  const lights = throttle((level) => call("set_lights", { level }), 150);
  $("lights").addEventListener("input", (e) => {
    $("lights-val").textContent = e.target.value;
    fill(e.target);
    lights(Number(e.target.value));
  });

  $("autostart").addEventListener("change", async (e) => {
    const r = await call("set_autostart_enabled", { enabled: e.target.checked });
    if (!r.ok) e.target.checked = !e.target.checked;
    else toast(r.value ? "Will start at login." : "Won’t start at login.");
  });
  const notif = () =>
    call("set_notifications", { lowPercent: settings.low_battery_percent, spareReady: settings.notify_spare_ready });
  $("low-battery").addEventListener("input", (e) => {
    settings.low_battery_percent = Number(e.target.value);
    $("low-battery-val").textContent = `${e.target.value}%`;
    fill(e.target);
  });
  $("low-battery").addEventListener("change", notif);
  $("spare-notify").addEventListener("change", (e) => {
    settings.notify_spare_ready = e.target.checked;
    notif();
  });
}

// --------------------------------------------------------------- boot

function renderAll() {
  renderDevice();
  renderBands();
  renderEqPresets();
  renderMic();
  renderChatMix();
  renderHeadset();
  renderTiles();
  drawEq();
}

async function boot() {
  buildBands();
  buildMic();
  buildChatMix();
  buildHeadset();
  $$('input[type="range"]').forEach(fill);

  const r = await call("status");
  if (!r.ok) return;
  snapshot = r.value.snapshot;
  settings = r.value.settings;
  $("autostart").checked = r.value.autostart;
  for (const p of r.value.mic_presets) micPresets[p.preset] = p;
  renderAll();
  refreshMic();
  refreshChatMix();

  await listen("device", (e) => {
    snapshot = e.payload;
    renderDevice();
  });
  await listen("settings", (e) => {
    // Changed from the tray menu.
    settings = e.payload;
    renderAll();
    refreshMic();
    refreshChatMix();
  });
  await listen("meter", (e) => onMeter(e.payload));
  await listen("chatmix", (e) => {
    if (!settings) return;
    settings.chatmix.balance = e.payload.balance;
    renderChatMix();
    renderTiles();
  });
}

boot();

// WebKit scrolls its focus target into view while the window maps.
window.addEventListener("load", () => document.querySelector("main").scrollTo(0, 0));
