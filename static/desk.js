const $ = (id) => document.getElementById(id);
const t = (k, vars) => (window.OQ_I18N && OQ_I18N.t ? OQ_I18N.t(k, vars) : k);
const fmt = (n, d = 2) => (n == null || Number.isNaN(Number(n)) ? "—" : Number(n).toFixed(d));
const moneyWhole = (n) =>
  n == null || Number.isNaN(Number(n)) ? "—" : "$" + Math.round(Number(n)).toLocaleString("en-US");

let busy = false;
let lastSnap = {};
let lastHouse = {};
let lastFills = [];
let lastGuide = {};
let lastMember = {};
let noteEmail = "";
let notes = [];
let seenFill = 0;
let lastAlloc = "";
let lastHalted = null;
let lastModeNoteAt = 0;
let warnedExpiry = false;
let hadFeed = false;
let paramsDirty = false;

function setText(id, v) {
  const el = $(id);
  if (el) el.textContent = v;
}

function notify(title, body) {
  const box = $("notify");
  if (!box) {
    window.alert((title ? title + " — " : "") + (body || ""));
    return;
  }
  $("notifyTitle").textContent = title || t("notify.title");
  $("notifyBody").textContent = body || "";
  box.hidden = false;
}
if ($("notifyOk")) $("notifyOk").onclick = () => { $("notify").hidden = true; };

function remaining(iso) {
  if (!iso) return t("status.expired");
  const ms = Date.parse(iso) - Date.now();
  if (Number.isNaN(ms) || ms <= 0) return t("status.expired");
  const s = Math.floor(ms / 1000);
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d}${t("time.d")} ${h}${t("time.h")}`;
  return `${h}${t("time.h")} ${m}${t("time.m")}`;
}

function remainingMs(iso) {
  if (!iso) return 0;
  const ms = Date.parse(iso) - Date.now();
  return Number.isNaN(ms) ? 0 : ms;
}

async function api(path, opts = {}) {
  const { headers, ...rest } = opts;
  const res = await fetch(path, {
    credentials: "same-origin",
    ...rest,
    headers: { "content-type": "application/json", ...(headers || {}) },
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    const err = new Error(data.error || res.statusText);
    err.need = data.need;
    err.have = data.have;
    throw err;
  }
  return data;
}

function isFree(me) {
  return !me || (!me.active && !me.admin);
}

function modeLabel(me) {
  if (isFree(me)) return "FREE";
  if (me.runStatus === "live" || (me.liveRunning && !me.halted)) return "LIVE";
  if (me.runStatus === "paper" || (me.paperRunning && !me.halted)) return "PAPER";
  return "STOPPED";
}

function paperOn(me) {
  return !!(me && (me.paperRunning || me.runStatus === "paper"));
}

function liveOn(me) {
  return !!(me && (me.liveRunning || me.runStatus === "live"));
}

function noteKey() {
  return "trade_notes_" + (noteEmail || "anon");
}

function loadNotes() {
  try {
    notes = JSON.parse(localStorage.getItem(noteKey()) || "[]");
  } catch {
    notes = [];
  }
}

function saveNotes() {
  localStorage.setItem(noteKey(), JSON.stringify(notes.slice(0, 50)));
}

function renderNotes() {
  const list = $("noteList");
  if (!list) return;
  list.innerHTML = notes
    .slice(0, 20)
    .map(
      (n) =>
        `<li class="${n.read ? "" : "unread"}"><strong>${n.kind}</strong>${n.text}<div class="when">${new Date(n.ts).toLocaleString()}</div></li>`
    )
    .join("") || "<li>No alerts yet.</li>";
}

function pushNote(kind, text) {
  if (!text) return;
  notes.unshift({ kind, text, ts: Date.now(), read: false });
  notes = notes.slice(0, 50);
  saveNotes();
  renderNotes();
  if (window.Notification && Notification.permission === "granted") {
    try {
      new Notification("OhioismQuant Trade", { body: text });
    } catch (_) {}
  }
}

function watchAlerts(me, snap, fills) {
  if (me.email && me.email !== noteEmail) {
    noteEmail = me.email;
    loadNotes();
    renderNotes();
    seenFill = 0;
    lastHalted = null;
    warnedExpiry = false;
  }
  const newest = (fills && fills[0] && fills[0].ts_ms) || 0;
  if (newest && newest !== seenFill) {
    if (seenFill) {
      const f = fills[0];
      pushNote("fill", `${f.kind} ${f.token} ${fmt(f.size, 1)} @ ${fmt(f.px, 3)}${f.paper ? " (paper)" : ""}`);
    }
    seenFill = newest;
  }
  const alloc = snap.alloc || me.lastAlloc || "";
  if (alloc && alloc !== lastAlloc) {
    if (lastAlloc) pushNote("alloc", alloc);
    lastAlloc = alloc;
  }
  const running = paperOn(me) || liveOn(me);
  if (lastHalted !== null && lastHalted !== running && Date.now() - lastModeNoteAt > 2500) {
    pushNote(
      "mode",
      liveOn(me) ? t("desk.noteLive") : paperOn(me) ? t("desk.notePaper") : t("desk.noteStop")
    );
    lastModeNoteAt = Date.now();
  }
  lastHalted = running;
  const left = remainingMs(me.accessUntil);
  if (me.active && left > 0 && left < 6 * 3600 * 1000 && !warnedExpiry) {
    warnedExpiry = true;
    pushNote("expiry", t("desk.noteExpiry", { left: remaining(me.accessUntil) }));
  }
  const feedOn = !!(snap && snap.ts_ms);
  if (hadFeed !== feedOn) {
    pushNote("feed", feedOn ? t("desk.noteFeedOn") : t("desk.noteFeedOff"));
    hadFeed = feedOn;
  }
}

function fieldBusy(id) {
  return document.activeElement === $(id);
}

function capsOf(guide) {
  const c = (guide && guide.caps) || {};
  return {
    plan: c.plan || lastMember.plan || "test",
    base: Number(c.base) || 5,
    maxNet: Number(c.maxNet) || 15,
    hedge: c.hedge !== false,
  };
}

function typedSize() {
  const caps = capsOf(lastGuide);
  const raw = $("setBase") ? String($("setBase").value || "").trim() : "";
  let base = raw === "" ? 5 : Number(raw);
  const hedge = $("setHedge") ? $("setHedge").checked : true;
  if (!Number.isFinite(base) || base <= 0) base = 5;
  const clampedBase = Math.min(caps.base, Math.max(5, Math.round(base)));
  return { base: clampedBase, hedge, rawBase: raw === "" ? 5 : base, caps };
}

function fundUsd(base) {
  const b = Math.max(5, Number(base) || 5);
  const t = Math.min(1, Math.max(0, (b - 5) / 45));
  return Math.round((200 + t * 6800) / 50) * 50;
}

function fillParams(me, guide) {
  const caps = capsOf(guide);
  if ($("bestLine")) {
    $("bestLine").textContent = t("desk.bestLine", { plan: caps.plan, base: caps.base });
  }
  if ($("setBase")) {
    $("setBase").max = String(caps.base);
    $("setBase").min = "5";
    $("setBase").step = "1";
    $("setBase").placeholder = "5";
  }
  if (paramsDirty) return;
  if (!fieldBusy("setBase") && $("setBase")) {
    const v = me.baseAmount != null && Number(me.baseAmount) >= 5 ? me.baseAmount : 5;
    $("setBase").value = v;
  }
  if ($("setHedge") && document.activeElement !== $("setHedge")) {
    $("setHedge").checked = me.hedge !== false;
  }
}

function renderCapital() {
  const typed = typedSize();
  if ($("needFund")) $("needFund").textContent = moneyWhole(fundUsd(typed.base));
}

function setHeadMode(me) {
  const el = $("headMode");
  if (!el) return;
  const m = modeLabel(me);
  el.textContent = t("mode." + m);
  el.classList.toggle("live", m === "LIVE");
  el.classList.toggle("halted", m === "STOPPED");
  el.classList.toggle("free", m === "FREE");
  document.querySelectorAll(".head-acct").forEach((n) => {
    n.hidden = m === "FREE";
  });
}

function setModeStatus(id, on, free) {
  const el = $(id);
  if (!el) return;
  el.textContent = t(on ? "status.running" : free ? "status.locked" : "status.stopped");
  el.classList.toggle("on", on);
  el.classList.toggle("off", !on);
}

function renderModeCards(me) {
  const paper = paperOn(me);
  const live = liveOn(me);
  const free = isFree(me);
  if ($("paperCard")) $("paperCard").classList.toggle("running", paper);
  if ($("liveCard")) $("liveCard").classList.toggle("running", live);
  setModeStatus("paperStatus", paper, free);
  setModeStatus("liveStatus", live, free);
  if ($("paperStart")) $("paperStart").disabled = !me.active || paper || busy;
  if ($("paperStop")) $("paperStop").disabled = !paper || busy;
  if ($("liveStart")) $("liveStart").disabled = !me.active || live || busy;
  if ($("liveStop")) $("liveStop").disabled = !live || busy;
  if ($("paperHint")) {
    $("paperHint").innerHTML = paper ? t("desk.paperHintOn") : t("desk.paperHintOff");
  }
  if ($("liveHint")) {
    if (!me.keysConnected) {
      $("liveHint").textContent = t("desk.liveNeedKeys");
    } else if (!me.liveReady) {
      $("liveHint").textContent = t("desk.liveNeedPk");
    } else {
      $("liveHint").textContent = t("desk.liveReadyHint");
    }
  }
}

async function loadBalance() {
  try {
    const b = await api("/api/balance");
    setText("acctCash", moneyWhole(b.cash));
    setText("acctPort", moneyWhole(b.portfolio));
    if (b.paperCashUsd != null) setText("paperCash", moneyWhole(b.paperCashUsd));
  } catch (_) {}
}

function render(me, snap, house, fills, guide) {
  lastMember = me || {};
  lastSnap = snap || {};
  lastHouse = house || {};
  lastFills = fills || [];
  lastGuide = guide || lastGuide || {};
  setText("plan", me.plan || "none");
  setText("left", remaining(me.accessUntil));
  setText("mode", t("mode." + modeLabel(me)));
  setText("paperCash", moneyWhole(me.paperCashUsd));
  setHeadMode(me);
  renderModeCards(me);
  if ($("keysStatus")) {
    $("keysStatus").textContent = me.liveReady
      ? t("desk.keysLive", { funder: me.funder || "wallet" })
      : me.keysConnected
        ? t("desk.keysApi", { funder: me.funder || "wallet" })
        : t("desk.keysNone");
  }
  if (me.funder && $("funder") && !$("funder").value) {
    $("funder").value = me.funder;
  }
  const testBar = $("testBar");
  if (testBar) {
    if (me.plan === "test" && me.active) {
      testBar.style.display = "flex";
      setText("testLeft", remaining(me.accessUntil));
    } else {
      testBar.style.display = "none";
    }
  }
  if ($("payBar")) $("payBar").style.display = me.active && me.plan === "month" ? "none" : "flex";
  if ($("deskMain")) $("deskMain").classList.toggle("is-locked", !me.active);
  const feed = $("feedNote");
  if (feed) {
    if (!me.active) {
      feed.textContent = t("desk.feedSub");
    } else if (liveOn(me)) {
      feed.textContent = lastSnap.ts_ms ? t("desk.feedLiveOn") : t("desk.feedLiveWait");
    } else if (paperOn(me)) {
      feed.textContent = lastSnap.ts_ms ? t("desk.feedPaperOn") : t("desk.feedPaperWait");
    } else {
      feed.textContent = t("desk.feedStop");
    }
  }
  fillParams(me, lastGuide);
  renderCapital();
  watchAlerts(me, lastSnap, lastFills);
}

async function refresh() {
  if (busy) return lastSnap;
  const data = await api("/api/desk");
  render(data.member, data.snap || {}, data.house || {}, data.fills || [], data.guide || {});
  loadBalance().catch(() => {});
  return data;
}

async function pay(plan) {
  if ($("payErr")) $("payErr").textContent = "";
  try {
    const out = await api("/api/pay", { method: "POST", body: JSON.stringify({ plan }) });
    if (out.invoiceUrl) location.href = out.invoiceUrl;
  } catch (e) {
    notify("Payment", e.message);
    if ($("payErr")) $("payErr").textContent = e.message;
  }
}

async function postSettings(body, okText) {
  try {
    busy = true;
    const out = await api("/api/settings", { method: "POST", body: JSON.stringify(body) });
    paramsDirty = false;
    render(out.member, lastSnap, lastHouse, lastFills, out.guide || lastGuide);
    notify("Size saved", okText);
    pushNote("params", okText);
  } catch (e) {
    notify("Could not save size", e.message);
  } finally {
    busy = false;
    refresh().catch(() => {});
  }
}

if ($("payTest")) $("payTest").onclick = () => pay("test");
if ($("payMonth")) $("payMonth").onclick = () => pay("month");
if ($("payMonth2")) $("payMonth2").onclick = () => pay("month");
async function syncPay(tries) {
  let out = { granted: false, member: null };
  for (let i = 0; i < tries; i++) {
    out = await api("/api/pay/sync", { method: "POST", body: "{}" });
    if (out.member && out.member.active) return out;
    await new Promise((r) => setTimeout(r, 2500));
  }
  return out;
}
if ($("saveKeys")) {
  $("saveKeys").onclick = async () => {
    try {
      const out = await api("/api/keys", {
        method: "POST",
        body: JSON.stringify({
          funder: $("funder").value.trim(),
          apiKey: $("apiKey").value.trim(),
          secret: $("secret").value.trim(),
          passphrase: $("passphrase").value.trim(),
          privateKey: $("privateKey").value.trim(),
          sigType: Number($("sigType").value || 0),
        }),
      });
      $("secret").value = "";
      $("passphrase").value = "";
      if ($("privateKey")) $("privateKey").value = "";
      if (out.restartLive) {
        notify(
          "Restart live",
          "New keys are saved. Live was still using the old keys, so live is off. Start live again to load the new keys."
        );
        pushNote("keys", "Keys changed while live. Live stopped — start live again.");
      } else if (out.member && out.member.liveReady) {
        notify("Keys saved", "API keys and private key encrypted. Start live to trade from the UK server.");
        pushNote("keys", "Keys on file. Start live to trade.");
      } else {
        notify("Keys saved", "API keys saved. Add the wallet private key to enable live.");
        pushNote("keys", "API keys saved. Private key still needed for live.");
      }
      if (out.member) render(out.member, lastSnap, lastHouse, lastFills, lastGuide);
      await refresh();
    } catch (e) {
      notify("Could not save keys", e.message);
    }
  };
}

function onSizeEdit() {
  paramsDirty = true;
  renderCapital();
}

if ($("setBase")) $("setBase").addEventListener("input", onSizeEdit);
if ($("setHedge")) $("setHedge").addEventListener("change", onSizeEdit);

if ($("saveParams")) {
  $("saveParams").onclick = () => {
    const t = typedSize();
    postSettings(
      { baseAmount: t.rawBase, hedge: t.hedge },
      "Your size is saved. Next wave uses " + t.base + " shares · " + moneyWhole(fundUsd(t.base)) + " fund."
    );
  };
}

if ($("enablePush")) {
  $("enablePush").onclick = async () => {
    if (!window.Notification) {
      notify("Alerts", "This browser cannot show desktop notifications.");
      pushNote("alerts", "This browser cannot show desktop notifications.");
      return;
    }
    const perm = await Notification.requestPermission();
    const text = perm === "granted" ? "Browser alerts on." : "Browser alerts blocked.";
    notify("Alerts", text);
    pushNote("alerts", text);
  };
}
if ($("clearNotes")) {
  $("clearNotes").onclick = () => {
    notes = [];
    saveNotes();
    renderNotes();
  };
}

async function setRun(mode, running) {
  try {
    busy = true;
    renderModeCards(lastMember);
    const out = await api("/api/run", {
      method: "POST",
      body: JSON.stringify({ mode, running }),
    });
    if (out.member) render(out.member, lastSnap, lastHouse, lastFills, lastGuide);
    const label = mode === "live" ? "Live" : "Paper";
    if (running) {
      pushNote("mode", label + " mode running.");
      location.href = "/dash";
      return;
    }
    notify(label, label + " stopped.");
    pushNote("mode", label + " stopped.");
  } catch (e) {
    if (e.need != null) {
      notify(
        "Not enough fund",
        "Necessary fund " + moneyWhole(e.need) + ". Wallet shows " + moneyWhole(e.have) + ". Deposit more pUSD before live."
      );
    } else {
      notify(mode === "live" ? "Live" : "Paper", e.message);
    }
    pushNote("error", e.message);
  } finally {
    busy = false;
    refresh().catch(() => {});
  }
}

if ($("paperStart")) $("paperStart").onclick = () => setRun("paper", true);
if ($("paperStop")) $("paperStop").onclick = () => setRun("paper", false);
if ($("liveStart")) $("liveStart").onclick = () => setRun("live", true);
if ($("liveStop")) $("liveStop").onclick = () => setRun("live", false);

(async () => {
  try {
    await refresh();
    const plan = new URLSearchParams(location.search).get("plan");
    if (plan === "test" || plan === "month") {
      history.replaceState({}, "", "/desk");
      await pay(plan);
    }
    const q = new URLSearchParams(location.search);
    if (q.get("paid") === "1" || q.get("order")) {
      const out = await syncPay(12);
      history.replaceState({}, "", "/desk");
      await refresh();
      if (out.member && out.member.active) {
        notify("You're in", "Access is active. Paper is on — open the dashboard to watch the book.");
      } else {
        notify("Payment", "Waiting for NOWPayments to confirm. This desk unlocks when the invoice clears.");
      }
    }
  } catch (e) {
    const msg = String(e.message || "");
    if (msg.toLowerCase().includes("sign in") || msg.includes("Unauthorized")) {
      location.href = "/login";
      return;
    }
    notify("Desk", msg);
    if ($("payErr")) $("payErr").textContent = msg;
  }
  setInterval(() => refresh().catch(() => {}), 1000);
  window.addEventListener("oq-lang", () => {
    if (lastMember && lastMember.email) {
      render(lastMember, lastSnap, lastHouse, lastFills, lastGuide);
    }
  });
})();
