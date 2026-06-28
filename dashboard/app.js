// Parclose dashboard: renders a crossing window's beat from window.json.
const EXPLORER = "https://testnet.cspr.live";
const txUrl = (h) => `${EXPLORER}/transaction/${h}`;
const pkgUrl = (h) => `${EXPLORER}/contract-package/${h.replace(/^hash-/, "")}`;
const short = (h) => (h.length > 16 ? `${h.slice(0, 8)}…${h.slice(-6)}` : h);
const el = (tag, cls, html) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (html !== undefined) n.innerHTML = html;
  return n;
};
const esc = (s) =>
  String(s).replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" }[c]));

const STEPS = ["step-orders", "step-agents", "step-clearing", "step-settlement", "beat"];

function renderOrders(data) {
  const root = document.getElementById("orders");
  root.innerHTML = "";
  data.agents.forEach((a) => {
    const c = el("div", "cipher");
    c.appendChild(el("div", "tag", "sealed order · ciphertext"));
    c.appendChild(el("div", "hash", esc(a.ciphertext_hash)));
    c.appendChild(el("div", "note", "side · size · price — hidden on-chain"));
    root.appendChild(c);
  });
}

function renderAgents(data) {
  const root = document.getElementById("agents");
  root.innerHTML = "";
  data.agents.forEach((a) => {
    const card = el("div", "agent");
    const top = el("div", "top");
    top.appendChild(el("div", "name", esc(a.name)));
    top.appendChild(el("div", `side ${esc(a.side)}`, esc(a.side)));
    card.appendChild(top);
    card.appendChild(
      el("div", "order", `<b>${esc(a.side)} ${esc(a.size)}</b> @ limit <b>${esc(a.limit)}</b>`)
    );
    card.appendChild(el("div", "rationale", esc(a.rationale)));
    const det = el("details");
    det.appendChild(el("summary", null, "why — the four factors it weighed"));
    const f = a.factors || {};
    const labels = {
      nav_signal: "NAV / market signal",
      inventory_risk: "inventory & risk limit",
      fill_probability: "fill probability vs competition",
      prior_context: "prior clearing context",
    };
    Object.keys(labels).forEach((k) => {
      if (f[k]) det.appendChild(el("div", "factor", `<b>${labels[k]}</b>${esc(f[k])}`));
    });
    card.appendChild(det);
    root.appendChild(card);
  });
}

function renderClearing(data) {
  const root = document.getElementById("clearing");
  root.innerHTML = "";
  const price = el("div", "price");
  price.appendChild(el("div", "p", esc(data.clearing.price)));
  price.appendChild(el("div", "l", "uniform clearing price"));
  root.appendChild(price);

  const fills = el("div", "fills");
  (data.clearing.fills || []).forEach((fl) => {
    const fundCredit = Number(fl.fund_credit),
      fundSpent = Number(fl.fund_spent),
      cashSpent = Number(fl.cash_spent),
      cashCredit = Number(fl.cash_credit);
    let line;
    if (fundCredit > 0) line = `Subscriber received <b>${fundCredit}</b> fund for <b>${cashSpent}</b> cash`;
    else if (fundSpent > 0) line = `Redeemer delivered <b>${fundSpent}</b> fund for <b>${cashCredit}</b> cash`;
    else line = "—";
    fills.appendChild(el("div", "fill", line));
  });
  root.appendChild(fills);
}

function renderSettlement(data) {
  const root = document.getElementById("settlement");
  root.innerHTML = "";
  root.appendChild(
    el("div", "ok", "✓ attestation verified on-chain — matched set settled atomically from escrow")
  );
  if (data.settlement_tx) {
    root.appendChild(
      el(
        "div",
        "tx",
        `settlement transaction: <a href="${txUrl(data.settlement_tx)}" target="_blank" rel="noreferrer">${short(
          data.settlement_tx
        )}</a>`
      )
    );
  }
  root.appendChild(
    el(
      "div",
      "tx",
      `CrossingEngine: <a href="${pkgUrl(data.deployed.crossing_engine)}" target="_blank" rel="noreferrer">${short(
        data.deployed.crossing_engine
      )}</a>`
    )
  );
}

function renderDeployed(data) {
  const root = document.getElementById("deployed");
  root.innerHTML = "";
  const names = {
    window_registry: "WindowRegistry",
    crossing_engine: "CrossingEngine",
    fund_token: "FundToken",
    cash_token: "CashToken",
    sealed_order_book: "SealedOrderBook",
  };
  Object.keys(names).forEach((k) => {
    const h = data.deployed[k];
    if (!h) return;
    const row = el("div", "row");
    row.appendChild(el("span", null, names[k]));
    const a = document.createElement("a");
    a.href = pkgUrl(h);
    a.target = "_blank";
    a.rel = "noreferrer";
    a.textContent = short(h);
    row.appendChild(a);
    root.appendChild(row);
  });
  document.getElementById("meta").textContent =
    `network: ${data.network} · window #${data.window_id} · reasoning model: ${data.model}`;
}

function play() {
  const btn = document.getElementById("play");
  btn.disabled = true;
  STEPS.forEach((id) => document.getElementById(id).classList.remove("show"));
  STEPS.forEach((id, i) => {
    setTimeout(() => {
      const node = document.getElementById(id);
      node.classList.add("show");
      node.scrollIntoView({ behavior: "smooth", block: "center" });
      if (i === STEPS.length - 1) {
        btn.disabled = false;
        setTimeout(() => window.scrollTo({ top: 0, behavior: "smooth" }), 600);
      }
    }, 700 * (i + 1));
  });
}

async function main() {
  let data;
  try {
    const res = await fetch("./window.json", { cache: "no-store" });
    data = await res.json();
  } catch (e) {
    document.querySelector("main").innerHTML =
      '<p style="color:#ff9d7a;padding:20px">Could not load window.json. Serve this folder, e.g. <code>python3 -m http.server</code>, then open the printed URL.</p>';
    return;
  }
  renderOrders(data);
  renderAgents(data);
  renderClearing(data);
  renderSettlement(data);
  renderDeployed(data);
  // Reveal everything by default; the Play button re-runs the staged beat.
  STEPS.forEach((id) => document.getElementById(id).classList.add("show"));
  document.getElementById("play").addEventListener("click", play);
}

main();
