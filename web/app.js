import { Rpc, addressUrl, isAddress } from "./chain.js";
import { TxError, Wallet, sendWrite } from "./tx.js";
import { $, LifecyclePanel, el, formatGen, link, parseGen, relative, short, when } from "./ui.js";
import { CONTRACT, MSG_FEES, REPO_URL } from "./config.js";

const rpc = new Rpc();
const wallet = new Wallet();
const READER = "0x0000000000000000000000000000000000000001";
let busy = false;
let cache = [];

const read = async (method, args = []) => {
  const r = await rpc.simulate(wallet.account ?? READER, CONTRACT, method, args, "read");
  if (!r.ok) throw new Error(r.error);
  return r.value;
};
const same = (a, b) => Boolean(a && b) && a.toLowerCase() === b.toLowerCase();

// ---------------------------------------------------------------- wallet

async function refreshWallet() {
  const btn = $("connect");
  if (!wallet.available) {
    btn.textContent = "No wallet found";
    btn.disabled = true;
    return;
  }
  if (wallet.account) {
    const bal = BigInt(await rpc.call("eth_getBalance", [wallet.account, "latest"]));
    btn.textContent = `${short(wallet.account)} · ${formatGen(bal, 2)}`;
  } else btn.textContent = "Connect wallet";
}

$("connect").onclick = async () => {
  try { await wallet.connect(); } catch (e) { toast(e.message || String(e), true); }
  refreshWallet();
  render();
};
wallet.onChange(() => { refreshWallet(); render(); });

function toast(text, bad = false) {
  const t = $("toast");
  t.textContent = text;
  t.className = bad ? "toast bad" : "toast";
  t.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (t.hidden = true), 7000);
}

async function run(title, opts, after) {
  if (busy) return toast("Another transaction is in progress.", true);
  if (!wallet.account) {
    try { await wallet.connect(); await refreshWallet(); } catch (e) { return toast(e.message, true); }
  }
  busy = true;
  const panel = new LifecyclePanel($("activity"), title);
  $("activity").scrollIntoView({ behavior: "smooth", block: "start" });
  try {
    const res = await sendWrite({ rpc, wallet, contract: CONTRACT, ...opts, onUpdate: (s) => panel.update(s) });
    panel.done(after ? after(res.preview) : "Done.");
  } catch (e) {
    panel.fail(e instanceof TxError ? e : new TxError("simulate", e.message || String(e)));
  } finally {
    busy = false;
    refreshWallet();
    render();
  }
}

// ---------------------------------------------------------------- create

$("f-check").onchange = () => {
  const evm = $("f-check").value === "evm-rpc";
  $("f-expect-wrap").hidden = evm;
  $("f-lag-wrap").hidden = !evm;
};

$("create-form").onsubmit = (ev) => {
  ev.preventDefault();
  let credit, bond;
  try {
    credit = parseGen($("f-credit").value);
    bond = parseGen($("f-bond").value);
  } catch (e) {
    return toast(e.message, true);
  }
  const customer = $("f-customer").value.trim();
  if (!isAddress(customer)) return toast("Customer must be a 0x address.", true);
  const evm = $("f-check").value === "evm-rpc";
  const args = [
    $("f-name").value.trim(),
    customer,
    $("f-url").value.trim(),
    $("f-check").value,
    evm ? "" : $("f-expect").value,
    evm ? Number($("f-lag").value) : 0,
    credit,
    Number($("f-cooldown").value) * 60,
    Number($("f-days").value),
  ];
  run(`Create SLA “${args[0]}”`, { method: "create", args, value: bond }, (s) =>
    `SLA #${s?.id} is active. Validators saw the endpoint up: ${s?.last_probe?.reason}.`);
};

// ---------------------------------------------------------------- cards

async function probeNow(s, out) {
  out.className = "hint";
  out.textContent = "A node is probing the endpoint…";
  // Simulate the customer's report: one node probes, nothing is sent or paid.
  const r = await rpc.simulate(s.customer, CONTRACT, "report", [s.id], "write");
  if (!r.ok) {
    out.textContent = `Cannot report right now: ${r.error}`;
    out.className = "hint bad";
    return;
  }
  const p = r.value?.last_probe;
  out.textContent = p?.down ? `A node sees it DOWN: ${p.reason}. A report now would pay ${formatGen(s.credit)} if validators agree.` : `A node sees it up: ${p?.reason}.`;
  out.className = p?.down ? "hint bad" : "hint ok";
}

async function showEvents(s, box) {
  box.replaceChildren(el("p", { class: "muted" }, "Loading…"));
  try {
    const evs = await read("events", [s.id]);
    box.replaceChildren(...(evs.length ? [el("ul", { class: "events" }, ...evs.map((e) =>
      el("li", {}, el("b", {}, e.event.replace("_", " ")), ` · ${when(e.at)}`,
        e.reason ? ` · ${e.reason}` : "",
        e.credit ? ` · credit ${formatGen(e.credit)}` : "",
        e.amount ? ` · ${formatGen(e.amount)}` : "")))] : [el("p", { class: "muted" }, "No events.")]));
  } catch (e) {
    box.replaceChildren(el("p", { class: "error" }, e.message));
  }
}

function card(s, now) {
  const me = wallet.account;
  const isCustomer = same(me, s.customer);
  const isProvider = same(me, s.provider);
  const status = s.status === "active" && s.expired ? "expired" : s.status;
  const lp = s.last_probe;
  const out = el("p", { class: "hint" });
  const events = el("div");
  const actions = el("div", { class: "actions" },
    el("button", { onclick: () => probeNow(s, out) }, "Check now"),
    el("button", { onclick: () => showEvents(s, events) }, "History"));
  if (s.status === "active" && !s.expired && isCustomer) {
    actions.append(
      el("button", { class: "primary", onclick: () => run(`Report outage on SLA #${s.id}`, { method: "report", args: [s.id], msgFees: MSG_FEES }, (x) =>
        x?.last_probe?.down ? `Validators agreed it is down (${x.last_probe.reason}). ${formatGen(s.credit)} credited; it arrives when this transaction finalizes.` : `Validators saw it up (${x?.last_probe?.reason}). No credit.`) }, "Report outage"),
      el("button", { onclick: () => confirm("Release the provider early? No more credits can be claimed.") && run(`Release SLA #${s.id}`, { method: "release", args: [s.id] }) }, "Release early"));
  }
  if (s.status === "active" && isProvider) {
    actions.append(el("button", { onclick: () => {
      const a = prompt(`Add GEN to the bond of SLA #${s.id}:`, "1");
      if (!a) return;
      try { run(`Top up SLA #${s.id}`, { method: "top_up", args: [s.id], value: parseGen(a) }); } catch (e) { toast(e.message, true); }
    } }, "Top up bond"));
  }
  if (isProvider && (s.status === "released" || (s.status === "active" && s.expired))) {
    actions.append(el("button", { class: "primary", onclick: () => run(`Withdraw bond of SLA #${s.id}`, { method: "withdraw", args: [s.id], msgFees: MSG_FEES }, () => `${formatGen(s.bond)} returns to you when this transaction finalizes.`) }, "Withdraw bond"));
  }
  const host = (() => { try { return new URL(s.url).host; } catch { return s.url; } })();
  const facts = el("dl", { class: "facts" },
    el("dt", {}, "Endpoint"), el("dd", {}, link(s.url, host), ` · ${s.check === "evm-rpc" ? `EVM RPC, head ≤ ${s.max_lag_s}s old` : `HTTP 2xx/3xx${s.expect ? `, contains “${s.expect}”` : ""}`}`),
    el("dt", {}, "Bond left"), el("dd", {}, formatGen(s.bond)),
    el("dt", {}, "Credit"), el("dd", {}, `${formatGen(s.credit)} per outage, at most every ${Math.round(s.cooldown_s / 60)} min`),
    el("dt", {}, "Incidents"), el("dd", {}, `${s.incidents} paid of ${s.probes} probes`),
    el("dt", {}, "Last probe"), el("dd", {}, lp ? `${lp.down ? "down" : "up"} · ${lp.reason} · ${when(lp.at)}` : "—"),
    el("dt", {}, "Term"), el("dd", {}, `ends ${when(s.term_end)} (${relative(s.term_end, now)})`),
    el("dt", {}, "Provider"), el("dd", {}, link(addressUrl(s.provider), short(s.provider)), isProvider ? " (you)" : ""),
    el("dt", {}, "Customer"), el("dd", {}, link(addressUrl(s.customer), short(s.customer)), isCustomer ? " (you)" : ""),
  );
  if (Number(s.next_report_at) > now) facts.append(el("dt", {}, "Next report"), el("dd", {}, `${when(s.next_report_at)} (cooldown)`));
  return el("article", { class: "item" },
    el("header", {},
      el("span", { class: `badge ${lp?.down ? "down" : status}` }, lp?.down && status === "active" ? "Down at last probe" : status[0].toUpperCase() + status.slice(1)),
      el("span", { class: "muted" }, `#${s.id}`)),
    el("h3", {}, s.name),
    facts, actions, out, events);
}

async function render() {
  try {
    const [list, stats] = await Promise.all([read("list"), read("stats")]);
    $("s-count").textContent = stats.slas;
    $("s-bonded").textContent = formatGen(stats.bonded, 2);
    $("s-credited").textContent = formatGen(stats.credited, 2);
    cache = list.items ?? [];
    const now = Date.now() / 1000;
    $("slas").replaceChildren(...(cache.length ? cache.map((s) => card(s, now)) : [el("p", { class: "muted" }, "No SLAs yet. Create the first one below.")]));
  } catch (e) {
    $("slas").replaceChildren(el("p", { class: "error" }, "Could not read the contract: " + e.message));
  }
}

$("contract-link").href = addressUrl(CONTRACT);
$("contract-link").textContent = short(CONTRACT);
$("repo-link").href = REPO_URL;
if (!isAddress(CONTRACT)) toast("config.js has no contract address.", true);
wallet.restore().then(refreshWallet).catch(() => {});
render();
setInterval(render, 30_000);
