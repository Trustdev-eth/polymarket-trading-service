const $ = (id) => document.getElementById(id);
const t = (k, vars) => (window.OQ_I18N && OQ_I18N.t ? OQ_I18N.t(k, vars) : k);

async function api(path, opts = {}) {
  const res = await fetch(path, {
    credentials: "same-origin",
    headers: { "content-type": "application/json", ...(opts.headers || {}) },
    ...opts,
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || res.statusText);
  return data;
}

function money(n) {
  return "$" + Number(n || 0).toFixed(2);
}

function remaining(iso) {
  if (!iso) return t("status.expired");
  const ms = Date.parse(iso) - Date.now();
  if (Number.isNaN(ms)) return "—";
  if (ms <= 0) return t("status.expired");
  const s = Math.floor(ms / 1000);
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d}${t("time.d")} ${h}${t("time.h")}`;
  return `${h}${t("time.h")} ${m}${t("time.m")}`;
}

function metric(label, value, hint) {
  return `<div class="metric"><span>${label}</span><b>${value}</b>${hint ? `<em>${hint}</em>` : ""}</div>`;
}

function pill(text, kind) {
  return `<span class="admin-pill${kind ? " " + kind : ""}">${text}</span>`;
}

function paidStatus(status) {
  return /finished|confirmed|sending|paid/i.test(status || "");
}

let members = [];
let orders = [];
let filter = "all";
let tab = "traders";

function visibleMembers() {
  const q = ($("q")?.value || "").trim().toLowerCase();
  return members.filter((m) => {
    const hay = `${m.email} ${m.tradeReferral || ""} ${m.plan}`.toLowerCase();
    if (q && !hay.includes(q)) return false;
    if (filter === "test") return m.plan === "test" && m.active;
    if (filter === "month") return m.plan === "month" && m.active;
    if (filter === "live") return m.live && m.active;
    if (filter === "paper") return m.paper && !m.live && m.active;
    if (filter === "halted") return m.halted;
    if (filter === "keys") return m.keysConnected;
    if (filter === "expiring") {
      if (!m.accessUntil) return false;
      const ms = Date.parse(m.accessUntil) - Date.now();
      return ms > 0 && ms < 48 * 3600 * 1000;
    }
    return true;
  });
}

function renderMembers() {
  const rows = visibleMembers();
  $("rows").innerHTML =
    rows
      .map((m) => {
        const mode = m.halted || m.runStatus === "stopped"
          ? pill("stopped", "bad")
          : m.liveRunning || m.runStatus === "live"
            ? pill("live", "ok")
            : m.active
              ? pill("paper", "ok")
              : pill("off");
        const haltLabel = m.halted ? "Resume" : "Halt";
        const haltVal = m.halted ? "false" : "true";
        return `<tr>
          <td><strong>${m.email}</strong></td>
          <td>${m.plan || "none"}</td>
          <td title="${m.accessUntil || ""}">${remaining(m.accessUntil)}</td>
          <td>${mode}${m.lastAlloc ? `<div class="note" style="margin:4px 0 0">${m.lastAlloc}</div>` : ""}</td>
          <td>${m.keysConnected ? pill("yes", "ok") : pill("no")}</td>
          <td>${money(m.tradePaidUsd)}</td>
          <td>${money(m.partnerUsd)}</td>
          <td>${m.tradeReferral || ""}</td>
          <td><button class="btn btn-ghost btn-sm" data-halt="${m.email}" data-halted="${haltVal}">${haltLabel}</button></td>
        </tr>`;
      })
      .join("") || `<tr><td colspan="9">No traders match.</td></tr>`;
  $("rows").querySelectorAll("[data-halt]").forEach((btn) => {
    btn.onclick = async () => {
      $("admErr").textContent = "";
      try {
        await api("/api/admin/halt", {
          method: "POST",
          body: JSON.stringify({
            email: btn.dataset.halt,
            halted: btn.dataset.halted === "true",
          }),
        });
        await load();
      } catch (err) {
        $("admErr").textContent = err.message;
      }
    };
  });
}

function renderOrders() {
  $("payRows").innerHTML =
    orders
      .map((o) => {
        const when = o.paidAt || o.createdAt || "";
        const ok = paidStatus(o.status);
        return `<tr>
          <td>${when ? new Date(when).toISOString().slice(0, 16).replace("T", " ") : "—"}</td>
          <td>${o.email}</td>
          <td>${o.plan}</td>
          <td>${money(o.amountUsd)}${o.payCurrency ? " · " + o.payCurrency : ""}</td>
          <td>${money(o.partnerUsd)}</td>
          <td>${pill(o.status || "waiting", ok ? "ok" : "")}</td>
          <td>${o.referral || ""}</td>
          <td>${o.invoiceUrl ? `<a class="text-link" href="${o.invoiceUrl}" target="_blank" rel="noopener">Invoice</a>` : "—"}</td>
        </tr>`;
      })
      .join("") || `<tr><td colspan="8">No NOWPayments invoices yet.</td></tr>`;
}

function renderOverview(o) {
  $("stats").innerHTML = [
    metric("Active", o.active, "non-admin"),
    metric("Test", o.test, "$50 / 3d"),
    metric("Month", o.month, "$800 / 30d"),
    metric("Live", o.live, "keys posting"),
    metric("Paper", o.paper ?? "—", "twin only"),
    metric("Halted", o.halted ?? "—", "paused"),
    metric("Keys", o.keysConnected ?? "—", "connected"),
    metric("Expiring 48h", o.expiring48h ?? "—", "renew soon"),
  ].join("");
  $("money").innerHTML = [
    metric("Gross", money(o.grossUsd), `${o.paidOrders || 0} paid`),
    metric("Partner 15%", money(o.partnerUsd), "all invoices"),
    metric("Test gross", money(o.grossTestUsd), money(o.partnerTestUsd) + " partner"),
    metric("Month gross", money(o.grossMonthUsd), money(o.partnerMonthUsd) + " partner"),
    metric("Waiting", o.waitingOrders ?? "—", "unpaid invoices"),
  ].join("");
  const h = o.house || {};
  $("house").innerHTML = [
    metric("Ŝ", h.s_hat != null ? Number(h.s_hat).toFixed(2) : "—"),
    metric("Yes bid", h.yes_bid != null ? Number(h.yes_bid).toFixed(3) : "—"),
    metric("Yes ask", h.yes_ask != null ? Number(h.yes_ask).toFixed(3) : "—"),
    metric("τ", h.tau_s != null ? Number(h.tau_s).toFixed(0) + "s" : "—"),
    metric("Net", h.net != null ? Number(h.net).toFixed(2) : "—"),
    metric("Mark PnL", h.mark_pnl != null ? Number(h.mark_pnl).toFixed(2) : "—"),
    metric("Quote", h.quote || "—"),
  ].join("");
}

function showTab(next) {
  tab = next;
  $("tab-traders").hidden = tab !== "traders";
  $("tab-payments").hidden = tab !== "payments";
  $("tabs").querySelectorAll("button").forEach((b) => {
    b.classList.toggle("active", b.dataset.tab === tab);
  });
}

async function load() {
  const [overview, memberRes, orderRes] = await Promise.all([
    api("/api/admin/overview"),
    api("/api/admin/members"),
    api("/api/admin/orders"),
  ]);
  members = memberRes.members || [];
  orders = orderRes.orders || [];
  renderOverview(overview);
  renderMembers();
  renderOrders();
}

$("tabs").querySelectorAll("button").forEach((b) => {
  b.onclick = () => showTab(b.dataset.tab);
});
$("filters").querySelectorAll("button").forEach((b) => {
  b.onclick = () => {
    filter = b.dataset.filter;
    $("filters").querySelectorAll("button").forEach((x) => {
      x.classList.toggle("active", x === b);
    });
    renderMembers();
  };
});
$("q").addEventListener("input", renderMembers);

$("gift").onsubmit = async (e) => {
  e.preventDefault();
  $("admErr").textContent = "";
  $("admOk").textContent = "";
  try {
    await api("/api/admin/gift", {
      method: "POST",
      body: JSON.stringify({
        email: $("giftEmail").value.trim(),
        days: Number($("giftDays").value),
      }),
    });
    $("giftEmail").value = "";
    $("admOk").textContent = "Access granted.";
    await load();
  } catch (err) {
    $("admErr").textContent = err.message;
  }
};

$("haltAll").onclick = async () => {
  if (!confirm("Halt every non-admin trader?")) return;
  $("admErr").textContent = "";
  try {
    const r = await api("/api/admin/halt-all", { method: "POST", body: "{}" });
    $("admOk").textContent = `Halted ${r.matched ?? 0} traders.`;
    await load();
  } catch (err) {
    $("admErr").textContent = err.message;
  }
};

const logout = $("logout");
if (logout) {
  logout.onclick = async () => {
    await fetch("/api/auth/logout", { method: "POST", credentials: "same-origin" });
    location.href = "/";
  };
}

load().catch((e) => {
  $("admErr").textContent = e.message;
});
setInterval(() => load().catch(() => {}), 8000);
window.addEventListener("oq-lang", () => {
  renderMembers();
  renderOrders();
});
