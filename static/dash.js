const $ = (id) => document.getElementById(id);
const tr = (k, vars) => (window.OQ_I18N && OQ_I18N.t ? OQ_I18N.t(k, vars) : k);
const page = location.pathname.startsWith("/pnl") ? "pnl" : "dash";
const MONTHS = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
const DOW = ["S","M","T","W","T","F","S"];
const fmt = (n, d = 2) => (n == null || !isFinite(Number(n)) ? "—" : Number(n).toLocaleString(undefined, { minimumFractionDigits: d, maximumFractionDigits: d }));
const moneyWhole = (n) =>
  n == null || !isFinite(Number(n)) ? "—" : "$" + Math.round(Number(n)).toLocaleString("en-US");
const moneyPnl = (n) => {
  if (n == null || !isFinite(Number(n))) return "—";
  const v = Number(n);
  const abs = Math.abs(v).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  return (v > 0 ? "+" : v < 0 ? "−" : "") + "$" + abs;
};
const tlabel = (ts) => new Date(ts).toISOString().slice(11, 19);
const nz = (x) => { const n = Number(x); return isFinite(n) && n > 0 ? n : null; };
const bookPx = (a, b) => (isFinite(a) && isFinite(b) && (a > 0 || b > 0) ? `${fmt(a, 3)} / ${fmt(b, 3)}` : "—");

function notify(title, body) {
  const box = $("notify");
  if (!box) { window.alert((title ? title + " — " : "") + (body || "")); return; }
  $("notifyTitle").textContent = title || tr("notify.title");
  $("notifyBody").textContent = body || "";
  box.hidden = false;
}
if ($("notifyOk")) $("notifyOk").onclick = () => { $("notify").hidden = true; };

function isFree(me) {
  return !me || (!me.active && !me.admin);
}

function modeLabel(me) {
  if (isFree(me)) return "FREE";
  if (me.runStatus === "live" || me.liveRunning) return "LIVE";
  if (me.runStatus === "paper" || me.paperRunning) return "PAPER";
  return "STOPPED";
}

async function api(path) {
  const res = await fetch(path, { credentials: "same-origin", cache: "no-store" });
  const data = await res.json().catch(() => ({}));
  if (res.status === 401) {
    location.href = "/login?next=" + encodeURIComponent(location.pathname);
    throw new Error("Sign in first.");
  }
  if (!res.ok) throw new Error(data.error || res.statusText);
  return data;
}

function setMode(me) {
  const el = $("headMode");
  if (!el) return;
  const m = modeLabel(me);
  el.textContent = tr("mode." + m);
  el.classList.toggle("live", m === "LIVE");
  el.classList.toggle("halted", m === "STOPPED");
  el.classList.toggle("free", m === "FREE");
  document.querySelectorAll(".head-acct").forEach((n) => {
    n.hidden = m === "FREE";
  });
  const fills = $("fillSection");
  if (fills) fills.hidden = m !== "LIVE";
}

function setRunBanner(me) {
  const el = $("runBanner");
  if (!el) return;
  const m = modeLabel(me);
  el.classList.toggle("running", m === "PAPER" || m === "LIVE");
  el.classList.toggle("stopped", m === "STOPPED" || m === "FREE");
  if (m === "LIVE") {
    el.innerHTML = tr("banner.live");
  } else if (m === "PAPER") {
    el.innerHTML = tr("banner.paper");
  } else if (m === "FREE") {
    el.innerHTML = tr("banner.free");
  } else {
    el.innerHTML = tr("banner.stop");
  }
}

function paintPnlEl(el, n) {
  if (!el) return;
  el.textContent = moneyPnl(n);
  el.classList.toggle("pos", Number(n) > 0);
  el.classList.toggle("neg", Number(n) < 0);
}

async function loadBalance() {
  try {
    const b = await api("/api/balance");
    const live = b.mode === "live" && !b.paper;
    const wrap = $("acctBalWrap");
    if (wrap) wrap.hidden = !live;
    if (live) {
      if ($("acctCash")) $("acctCash").textContent = moneyWhole(b.balance != null ? b.balance : b.cash);
    }
    paintPnlEl($("acctPnl"), b.pnl);
    if ($("deskPnl")) paintPnlEl($("deskPnl"), b.pnl);
    if ($("pnlSession")) paintPnlEl($("pnlSession"), b.pnl);
    const pnlBal = $("pnlBalWrap");
    if (pnlBal) {
      pnlBal.hidden = !live;
      if (live && $("pnlBal")) $("pnlBal").textContent = moneyWhole(b.balance != null ? b.balance : b.cash);
    }
  } catch (_) {}
}

if ($("logout")) {
  $("logout").onclick = async () => {
    await fetch("/api/auth/logout", { method: "POST", credentials: "same-origin" });
    location.href = "/";
  };
}

document.querySelectorAll(".head-pages a").forEach((a) => {
  a.classList.toggle("on", a.getAttribute("href") === "/" + page || (page === "dash" && a.getAttribute("href") === "/dash"));
});

let WINDOW_MS = 8 * 60 * 1000;
const BUCKET_MS = 400;
const DRAW_MS = 50;
const EASE_MS = 220;
const POLL_MS = 400;
const LTTB_N = 220;
const SERIES_KEYS = ["binance", "chainlink", "twap", "yes_bid", "yes_ask", "no_bid", "no_ask", "s_hat"];

let btc, yn, sig, eqChart, chartsReady = false, current = "", drawRaf = 0, lastDraw = 0;
let buf = [];
let want = null;
let view = null;
let lastStatKey = "";
let userZoomed = false;

function isLight() {
  return document.documentElement.getAttribute("data-theme") === "light";
}
function chartTheme() {
  if (isLight()) {
    return {
      tick: "#63716b",
      grid: "rgba(23,36,30,0.08)",
      tooltipBg: "#ffffff",
      border: "#dee6e0",
      body: "#17241e",
      legend: "#63716b",
      title: "#63716b",
      empty: "#ebedf0",
      month: "#63716b",
    };
  }
  return {
    tick: "#8aa196",
    grid: "rgba(255,255,255,0.06)",
    tooltipBg: "#101816",
    border: "#24332c",
    body: "#f4faf6",
    legend: "#9aada3",
    title: "#9aada3",
    empty: "#161b22",
    month: "#8aa196",
  };
}
function chartCommon() {
  const th = chartTheme();
  const axis = {
    ticks: { color: th.tick, maxTicksLimit: 7, maxRotation: 0, minRotation: 0, font: { size: 11 } },
    grid: { color: th.grid },
    border: { display: false },
  };
  return {
    animation: false,
    parsing: false,
    spanGaps: true,
    responsive: true,
    maintainAspectRatio: false,
    interaction: { mode: "index", axis: "x", intersect: false },
    elements: { point: { radius: 0, hoverRadius: 4 }, line: { tension: 0.25 } },
    plugins: {
      decimation: { enabled: false },
      legend: {
        labels: { color: th.legend, boxWidth: 10, boxHeight: 2, usePointStyle: true, pointStyle: "line", font: { size: 11 }, padding: 12 },
      },
      tooltip: {
        backgroundColor: th.tooltipBg,
        borderColor: th.border,
        borderWidth: 1,
        titleColor: th.title,
        bodyColor: th.body,
        padding: 10,
        callbacks: {
          title: (items) => (items[0] ? tlabel(items[0].parsed.x) : ""),
          label: (item) => `${item.dataset.label}: ${fmt(item.parsed.y, item.parsed.y < 2 ? 3 : 2)}`,
        },
      },
      zoom: {
        pan: { enabled: true, mode: "x", modifierKey: undefined, onPanStart: () => { userZoomed = true; } },
        zoom: {
          wheel: { enabled: true },
          pinch: { enabled: true },
          mode: "x",
          onZoomStart: () => { userZoomed = true; },
        },
        limits: { x: { minRange: 20_000 } },
      },
    },
    scales: {
      x: {
        ...axis,
        type: "linear",
        ticks: { ...axis.ticks, callback: (v) => tlabel(v) },
      },
      y: { ...axis, ticks: { ...axis.ticks, maxTicksLimit: 5 } },
    },
  };
}
function applyChartTheme() {
  const th = chartTheme();
  const paint = (c) => {
    if (!c) return;
    c.options.plugins.legend.labels.color = th.legend;
    c.options.plugins.tooltip.backgroundColor = th.tooltipBg;
    c.options.plugins.tooltip.borderColor = th.border;
    c.options.plugins.tooltip.titleColor = th.title;
    c.options.plugins.tooltip.bodyColor = th.body;
    ["x", "y"].forEach((ax) => {
      if (!c.options.scales[ax]) return;
      c.options.scales[ax].ticks.color = th.tick;
      c.options.scales[ax].grid.color = th.grid;
    });
    c.update("none");
  };
  paint(btc);
  paint(yn);
  paint(sig);
  paint(eqChart);
}

function line(id, datasets, extra) {
  return new Chart(document.getElementById(id), {
    type: "line",
    data: { labels: [], datasets },
    options: { ...chartCommon(), ...extra },
  });
}

function dsLine(label, color, extra = {}) {
  return {
    label,
    data: [],
    borderColor: color,
    backgroundColor: extra.fill ? color + "22" : "transparent",
    borderWidth: extra.borderWidth || 2,
    pointRadius: 0,
    tension: 0.08,
    cubicInterpolationMode: "default",
    borderJoinStyle: "round",
    borderCapStyle: "round",
    spanGaps: true,
    fill: extra.fill || false,
    ...extra,
  };
}

function ensureCharts() {
  if (chartsReady || page !== "dash" || typeof Chart === "undefined") return;
  try {
    btc = line("cBtc", [
      dsLine("Binance", "#f5c451"),
      dsLine("Chainlink", "#7dd3fc"),
      dsLine("TWAP", "#6ee7b7", { borderDash: [5, 4], borderWidth: 1.5 }),
    ]);
    const common = chartCommon();
    yn = line("cYn", [
      dsLine("Yes bid", "#34d399"),
      dsLine("Yes ask", "#6ee7b7", { borderDash: [4, 3], borderWidth: 1.25 }),
      dsLine("No bid", "#fb7185"),
      dsLine("No ask", "#fda4af", { borderDash: [4, 3], borderWidth: 1.25 }),
    ], { scales: { ...common.scales, y: { ...common.scales.y, min: 0, max: 1 } } });
    sig = line("cSig", [dsLine("Ŝ", "#e879f9")]);
    chartsReady = true;
  } catch (_) {}
}

function ease(a, b, k) {
  if (b == null) return a;
  if (a == null) return b;
  return a + (b - a) * k;
}

function lttb(points, n) {
  const len = points.length;
  if (len <= n || n < 3) return points;
  const sampled = [points[0]];
  const bucket = (len - 2) / (n - 2);
  let a = 0;
  for (let i = 0; i < n - 2; i++) {
    const start = Math.floor((i + 1) * bucket) + 1;
    const end = Math.min(Math.floor((i + 2) * bucket) + 1, len);
    let avgX = 0, avgY = 0, count = Math.max(end - start, 1);
    for (let j = start; j < end; j++) {
      avgX += points[j].x;
      avgY += points[j].y;
    }
    avgX /= count;
    avgY /= count;
    const rangeOff = Math.floor(i * bucket) + 1;
    const rangeTo = Math.floor((i + 1) * bucket) + 1;
    const pa = points[a];
    let maxArea = -1, nextA = rangeOff;
    for (let j = rangeOff; j < rangeTo; j++) {
      const area = Math.abs((pa.x - avgX) * (points[j].y - pa.y) - (pa.x - points[j].x) * (avgY - pa.y));
      if (area > maxArea) {
        maxArea = area;
        nextA = j;
      }
    }
    sampled.push(points[nextA]);
    a = nextA;
  }
  sampled.push(points[len - 1]);
  return sampled;
}

function seriesXY(key) {
  const pts = [];
  for (const s of buf) {
    if (s[key] != null) pts.push({ x: s.ts, y: s[key] });
  }
  const reduced = lttb(pts, LTTB_N);
  if (view && view[key] != null) {
    const head = { x: view.ts, y: view[key] };
    const last = reduced[reduced.length - 1];
    if (!last || head.x >= last.x) reduced.push(head);
    else last.y = head.y;
  }
  return reduced;
}

function fitY(chart, padFrac, minPad, hard) {
  if (!chart) return;
  const ys = [];
  for (const ds of chart.data.datasets) {
    for (const p of ds.data) {
      if (p && Number.isFinite(p.y)) ys.push(p.y);
    }
  }
  if (!ys.length) return;
  let lo = Math.min.apply(null, ys);
  let hi = Math.max.apply(null, ys);
  if (hi - lo < minPad) {
    const mid = (hi + lo) / 2;
    lo = mid - minPad / 2;
    hi = mid + minPad / 2;
  }
  const pad = Math.max((hi - lo) * padFrac, minPad * 0.15);
  lo -= pad;
  hi += pad;
  const y = chart.options.scales.y;
  if (hard || y.min == null || y.max == null) {
    y.min = lo;
    y.max = hi;
    return;
  }
  y.min += (lo - y.min) * 0.35;
  y.max += (hi - y.max) * 0.35;
}

function trimBuf() {
  const floor = (view && view.ts ? view.ts : Date.now()) - Math.max(WINDOW_MS, 15 * 60 * 1000);
  while (buf.length && buf[0].ts < floor) buf.shift();
  while (buf.length > 1400) buf.shift();
}

function sampleTick(t) {
  const ts = Number(t.ts) || Date.now();
  return {
    ts,
    binance: nz(t.binance),
    chainlink: nz(t.chainlink),
    twap: nz(t.twap),
    yes_bid: nz(t.yes_bid),
    yes_ask: nz(t.yes_ask),
    no_bid: nz(t.no_bid),
    no_ask: nz(t.no_ask),
    s_hat: nz(t.s_hat),
  };
}

function ingest(t, history) {
  if (!t) return;
  if (t.slug && current && t.slug !== current) {
    current = t.slug;
    if (want) want.yes_bid = want.yes_ask = want.no_bid = want.no_ask = null;
    if (view) view.yes_bid = view.yes_ask = view.no_bid = view.no_ask = null;
  } else if (t.slug) {
    current = t.slug;
  }
  const s = sampleTick(t);
  if (history) {
    const last = buf[buf.length - 1];
    if (last && s.ts <= last.ts) buf[buf.length - 1] = s;
    else buf.push(s);
    return;
  }
  setStats(t);
  want = s;
  if (!view) view = { ...s };
  const last = buf[buf.length - 1];
  if (!last) buf.push({ ...s });
  else if (s.ts - last.ts >= BUCKET_MS) {
    buf.push({ ...s });
    trimBuf();
  }
}

function xWindow() {
  const now = Math.max(Date.now(), view && view.ts || 0, want && want.ts || 0);
  const first = buf.length ? buf[0].ts : now;
  const age = Math.max(0, now - first);
  const span = Math.min(WINDOW_MS, Math.max(age * 1.04 + 2000, 8_000));
  return { now, min: now - span, max: now + Math.max(600, span * 0.02) };
}

function paint() {
  if (!chartsReady || (!view && !buf.length)) return;
  const win = xWindow();
  const applyX = (c) => {
    if (!c || userZoomed) return;
    c.options.scales.x.min = win.min;
    c.options.scales.x.max = win.max;
  };
  if (btc) {
    btc.data.datasets[0].data = seriesXY("binance");
    btc.data.datasets[1].data = seriesXY("chainlink");
    btc.data.datasets[2].data = seriesXY("twap");
    fitY(btc, 0.12, 8);
    applyX(btc);
    btc.update("none");
  }
  if (yn) {
    yn.data.datasets[0].data = seriesXY("yes_bid");
    yn.data.datasets[1].data = seriesXY("yes_ask");
    yn.data.datasets[2].data = seriesXY("no_bid");
    yn.data.datasets[3].data = seriesXY("no_ask");
    applyX(yn);
    yn.update("none");
  }
  if (sig) {
    sig.data.datasets[0].data = seriesXY("s_hat");
    fitY(sig, 0.12, 8);
    applyX(sig);
    sig.update("none");
  }
}

function startDrawLoop() {
  if (drawRaf) return;
  const step = (now) => {
    drawRaf = requestAnimationFrame(step);
    if (now - lastDraw < DRAW_MS) return;
    const dt = Math.min(80, lastDraw ? now - lastDraw : DRAW_MS);
    lastDraw = now;
    if (want && view) {
      const k = 1 - Math.exp(-dt / EASE_MS);
      for (const key of SERIES_KEYS) view[key] = ease(view[key], want[key], k);
      // Last print time, not wall clock — stretching y to Date.now() drew fake Chainlink flats.
      view.ts = Math.max(view.ts || 0, want.ts || 0);
    }
    paint();
  };
  drawRaf = requestAnimationFrame(step);
}

function setStats(t) {
  if (!$("stats")) return;
  const key = [t.binance, t.chainlink, t.s_hat, t.yes_bid, t.yes_ask, t.twap, t.net, t.mark_pnl, t.quote, t.tau_s].join("|");
  if (key === lastStatKey) return;
  lastStatKey = key;
  $("stats").innerHTML = [
    [tr("stat.binance"), fmt(t.binance, 2)],
    [tr("stat.chainlink"), fmt(t.chainlink, 2)],
    [tr("stat.shat"), fmt(t.s_hat, 2)],
    [tr("stat.yes"), bookPx(t.yes_bid, t.yes_ask)],
    [tr("stat.no"), bookPx(t.no_bid, t.no_ask)],
    [tr("stat.twap"), fmt(t.twap, 2)],
    [tr("stat.net"), fmt(t.net, 2)],
    [tr("stat.mark"), fmt(t.mark_pnl, 2)],
  ].map(([k, v]) => `<div class="dash-stat"><span>${k}</span><strong>${v}</strong></div>`).join("");
  if ($("meta")) {
    $("meta").innerHTML = [
      [tr("meta.market"), t.slug || "BTC 15m"],
      [tr("meta.tau"), Number(t.tau_s || 0).toFixed(0) + "s"],
      [tr("meta.regime"), t.regime || "—"],
      [tr("meta.quote"), t.quote || "—"],
      [tr("meta.k"), fmt(t.k_t, 2)],
      [tr("meta.alloc"), t.alloc || "—"],
    ].map(([k, v]) => `<span class="dash-chip"><i>${k}</i>${v}</span>`).join("");
  }
}

function resetCharts() {
  buf = [];
  want = null;
  view = null;
  lastStatKey = "";
  userZoomed = false;
  if (!chartsReady) return;
  [btc, yn, sig].forEach((c) => {
    if (!c) return;
    c.data.datasets.forEach((d) => { d.data = []; });
    if (!c.options.scales.y) return;
    if (c === yn) {
      c.options.scales.y.min = 0;
      c.options.scales.y.max = 1;
    } else {
      delete c.options.scales.y.min;
      delete c.options.scales.y.max;
    }
  });
}

function pushTick(t) {
  ingest(t, false);
}

function hasPx(t) {
  return nz(t.binance) || nz(t.chainlink) || nz(t.yes_bid) || nz(t.s_hat);
}

function bucketTicks(ticks) {
  const m = new Map();
  for (const t of ticks) {
    if (!hasPx(t)) continue;
    const ts = Number(t.ts);
    if (!ts) continue;
    const b = Math.floor(ts / BUCKET_MS) * BUCKET_MS;
    m.set(b, Object.assign({}, t, { ts: b }));
  }
  return Array.from(m.entries()).sort((a, b) => a[0] - b[0]).map((e) => e[1]);
}

async function loadHistory() {
  ensureCharts();
  const h = await api("/api/history");
  const ticks = bucketTicks(h.ticks || []);
  if (h.slug && !current) current = h.slug;
  const keep = want;
  const keepView = view;
  buf = [];
  for (const t of ticks) ingest(t, true);
  if (keep && (!buf.length || keep.ts >= buf[buf.length - 1].ts)) {
    want = keep;
    view = keepView || { ...keep };
  } else if (buf.length) {
    const last = buf[buf.length - 1];
    if (!want) want = { ...last };
    if (!view) view = { ...last };
  }
  const tail = ticks[ticks.length - 1];
  if (tail) setStats(tail);
}

async function pollLast() {
  try {
    const d = await api("/api/last");
    if (d.member) {
      setMode(d.member);
      setRunBanner(d.member);
      const gate = $("vipGate");
      if (gate && d.member && (d.member.active || d.member.admin)) gate.hidden = true;
    }
    const t = d.tick || {};
    if (d.snap) {
      t.net = d.snap.net;
      t.mark_pnl = d.snap.mark_pnl;
      t.alloc = d.snap.alloc;
    }
    pushTick(t);
    if (page === "dash" && modeLabel(d.member || lastMe) === "LIVE") renderFills(d.fills || []);
  } catch (_) {}
}

function utcDay(d) {
  return new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate()));
}

function heatFill(pnl, maxAbs, empty) {
  if (!pnl) return empty;
  const t = Math.min(1, Math.abs(pnl) / Math.max(maxAbs, 0.01));
  const a = 0.22 + t * 0.78;
  if (pnl > 0) return `rgba(57, 211, 83, ${a})`;
  return `rgba(248, 81, 73, ${a})`;
}

function renderHeat(days, selected, onPick, startMs) {
  const host = $("heat");
  if (!host) return;
  const today = utcDay(new Date());
  const map = days || {};
  const keys = Object.keys(map).sort();
  let origin = startMs > 0 ? utcDay(new Date(startMs)) : today;
  if (keys.length) {
    const first = utcDay(new Date(keys[0] + "T00:00:00Z"));
    if (first < origin) origin = first;
  }
  const start = utcDay(origin);
  start.setUTCDate(start.getUTCDate() - start.getUTCDay());
  const cols = Math.max(1, Math.floor((today - start) / (7 * 86400000)) + 1);
  const cell = cols <= 8 ? 14 : cols <= 26 ? 12 : 11;
  const gap = 3, left = 18, top = 18;
  const th = chartTheme();
  let maxAbs = 0;
  keys.forEach((k) => {
    maxAbs = Math.max(maxAbs, Math.abs(Number(map[k].pnl) || 0));
  });
  const w = left + cols * (cell + gap);
  const h = top + 7 * (cell + gap) + 4;
  let svg = `<svg class="heat-svg" width="${w}" height="${h}">`;
  DOW.forEach((d, i) => {
    if (i % 2) svg += `<text x="0" y="${top + i * (cell + gap) + 9}" fill="${th.month}" font-size="9">${d}</text>`;
  });
  let lastMonth = -1;
  for (let c = 0; c < cols; c++) {
    for (let r = 0; r < 7; r++) {
      const d = new Date(start);
      d.setUTCDate(d.getUTCDate() + c * 7 + r);
      if (d > today) continue;
      const key = d.toISOString().slice(0, 10);
      if (r === 0 && d.getUTCMonth() !== lastMonth) {
        lastMonth = d.getUTCMonth();
        svg += `<text x="${left + c * (cell + gap)}" y="12" fill="${th.month}" font-size="10">${MONTHS[lastMonth]}</text>`;
      }
      const x = left + c * (cell + gap);
      const y = top + r * (cell + gap);
      if (d < origin) {
        svg += `<rect x="${x}" y="${y}" width="${cell}" height="${cell}" rx="2" fill="${th.empty}" stroke="${th.border}" opacity="0.45"></rect>`;
        continue;
      }
      const rec = map[key];
      const pnl = rec ? Number(rec.pnl) || 0 : 0;
      const on = key === selected ? "stroke:#f5c451;stroke-width:1.6" : `stroke:${th.border}`;
      const title = rec ? `${key} · ${moneyPnl(pnl)} · ${rec.n} windows` : key;
      svg += `<rect data-day="${key}" x="${x}" y="${y}" width="${cell}" height="${cell}" rx="2" fill="${heatFill(rec ? pnl : 0, maxAbs, th.empty)}" ${on}><title>${title}</title></rect>`;
    }
  }
  svg += "</svg>";
  host.innerHTML = svg;
  host.querySelectorAll("rect[data-day]").forEach((el) => {
    el.addEventListener("click", () => onPick(el.getAttribute("data-day")));
  });
}

function renderDayTable(results, day) {
  const body = $("dayRows");
  if (!body) return;
  const rows = (results || []).filter((r) => {
    const k = new Date(r.ts_ms).toISOString().slice(0, 10);
    return k === day;
  });
  const sum = rows.reduce((s, r) => s + Number(r.pnl || 0), 0);
  if ($("dayLabel")) $("dayLabel").textContent = day || "—";
  if ($("daySum")) {
    $("daySum").textContent = rows.length ? moneyPnl(sum) : "—";
    $("daySum").classList.toggle("pos", sum > 0);
    $("daySum").classList.toggle("neg", sum < 0);
  }
  body.innerHTML = rows.length
    ? rows.map((r) => {
        const cls = r.pnl >= 0 ? "pos" : "neg";
        return `<tr><td>${new Date(r.ts_ms).toISOString().slice(11, 19)}</td><td>${r.slug || ""}</td><td class="${cls}">${moneyPnl(r.pnl)}</td></tr>`;
      }).join("")
    : `<tr><td colspan="3">${tr("pnl.emptyDay")}</td></tr>`;
}

function renderFills(fills) {
  const body = $("fillRows");
  if (!body) return;
  body.innerHTML = (fills || []).slice(0, 24).map((f) =>
    `<tr><td>${new Date(f.ts_ms).toISOString().slice(11, 19)}</td><td>${f.kind}</td><td>${f.token}</td><td>${fmt(f.size, 1)}</td><td>${fmt(f.px, 3)}</td></tr>`
  ).join("") || `<tr><td colspan="5">No fills yet.</td></tr>`;
}

function renderEquity(equity) {
  if (typeof Chart === "undefined" || !$("cEq")) return;
  const data = (equity || []).map((e) => ({ x: e.ts, y: e.cum != null ? e.cum : e.pnl }));
  const th = chartTheme();
  const last = data.length ? data[data.length - 1].y : 0;
  const color = last >= 0 ? "#3fb950" : "#f85149";
  if (!eqChart) {
    const opt = chartCommon();
    opt.plugins.legend.display = false;
    opt.plugins.tooltip.callbacks = {
      title: (items) => items[0] ? new Date(items[0].parsed.x).toISOString().replace("T", " ").slice(0, 19) : "",
      label: (item) => moneyPnl(item.parsed.y),
    };
    opt.scales.x.ticks.callback = (v) => {
      const d = new Date(v);
      return `${d.getUTCMonth() + 1}/${d.getUTCDate()} ${String(d.getUTCHours()).padStart(2, "0")}:${String(d.getUTCMinutes()).padStart(2, "0")}`;
    };
    eqChart = new Chart($("cEq"), {
      type: "line",
      data: { datasets: [dsLine("PnL", color, { fill: "origin", backgroundColor: color + "33" })] },
      options: opt,
    });
  }
  eqChart.data.datasets[0].data = data;
  eqChart.data.datasets[0].borderColor = color;
  eqChart.data.datasets[0].backgroundColor = color + "33";
  if (data.length) {
    eqChart.options.scales.x.min = data[0].x;
    eqChart.options.scales.x.max = data[data.length - 1].x;
  }
  fitY(eqChart, 0.18, 1, true);
  eqChart.update("none");
  const _ = th;
}

async function loadPnl() {
  const d = await api("/api/pnl");
  const days = d.days || {};
  const keys = Object.keys(days).sort();
  const startMs = Number(d.sessionStartedAt) || 0;
  let selected = keys.length ? keys[keys.length - 1] : new Date().toISOString().slice(0, 10);
  const pick = (day) => {
    selected = day;
    renderHeat(days, selected, pick, startMs);
    renderDayTable(d.results || [], selected);
  };
  renderHeat(days, selected, pick, startMs);
  renderDayTable(d.results || [], selected);
  renderEquity(d.equity || []);
  paintPnlEl($("pnlSession"), d.pnl);
  const live = d.mode === "live";
  const fills = $("fillSection");
  if (fills) {
    fills.hidden = !live;
    if (live) renderFills(d.fills || []);
  }
}

function bindRanges() {
  const host = $("chartRanges");
  if (!host) return;
  host.querySelectorAll("button").forEach((btn) => {
    btn.onclick = () => {
      WINDOW_MS = Number(btn.getAttribute("data-range")) || WINDOW_MS;
      userZoomed = false;
      host.querySelectorAll("button").forEach((b) => b.classList.toggle("on", b === btn));
      if (btc) btc.resetZoom && btc.resetZoom();
      if (yn) yn.resetZoom && yn.resetZoom();
      if (sig) sig.resetZoom && sig.resetZoom();
    };
  });
}

function vipBlocked() {
  const g = $("vipGate");
  return !!(g && !g.hidden);
}

let lastMe = {};
async function loadMe() {
  try {
    const me = await api("/api/me");
    lastMe = me || {};
    setMode(me);
    setRunBanner(me);
  } catch (_) {}
}

(async () => {
  try {
    await loadMe();
    if (vipBlocked()) return;
    await loadBalance();
    bindRanges();
    if (page === "dash") {
      ensureCharts();
      startDrawLoop();
      await loadHistory().catch(() => {});
      await pollLast();
      setInterval(pollLast, POLL_MS);
    } else {
      await loadPnl();
      setInterval(loadPnl, 8000);
    }
    setInterval(loadBalance, 8000);
    window.addEventListener("oq-theme", () => {
      applyChartTheme();
      if (page === "pnl") loadPnl().catch(() => {});
    });
    window.addEventListener("oq-lang", () => {
      if (lastMe) {
        setMode(lastMe);
        setRunBanner(lastMe);
      }
      lastStatKey = "";
      if (page === "dash" && buf.length) setStats(buf[buf.length - 1]);
    });
  } catch (e) {
    notify("Dashboard", e.message || String(e));
  }
})();
