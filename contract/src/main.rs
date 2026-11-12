//! SLA Escrow: uptime guarantees backed by GEN, judged by independent validators.
//!
//! A provider posts a bond for one customer and one endpoint:
//!   - `http`:    up when GET returns 2xx/3xx (and contains `expect`, if set)
//!   - `evm-rpc`: up when `eth_getBlockByNumber("latest")` answers and the head
//!                block is at most `max_lag_s` seconds old
//! When the customer's monitoring sees an outage, the customer calls `report(id)`.
//! The leader and every validator probe the endpoint from their own network.
//! Only if they agree it is down does the contract pay the agreed credit to
//! the customer, at most once per cooldown window. If they disagree (an
//! endpoint that flaps between probes) nothing is paid: the provider gets the
//! benefit of the doubt. Status pages written by the provider play no part.

use glkit::genlayer_sdk::abi::entry::MessageData;
use glkit::genlayer_sdk::abi::entry::contract_def::{ConsensusStageData, Contract, ContractExt};
use glkit::genlayer_sdk::storage::{DynArray, Root};
use glkit::*;

glkit::genlayer_sdk::record!(State {
    slas: Records,
    count: u32,
    bonded: U256,
    credited: U256,
    log: DynArray<Vec<u8>>,
});

fn state() -> State {
    Root::<State>::get().contract_instance().get()
}

const UA: &str = "sla-escrow/1.0 (+https://github.com/brossXnft/genlayer-sla-escrow)";
const MIN_COOLDOWN: u64 = 600;
const MAX_COOLDOWN: u64 = 7 * 86_400;

fn sid(n: u64) -> String {
    format!("{n:08}")
}

fn num_i64(v: &Value, k: &str) -> i64 {
    f_i64(v, k)
}

// ------------------------------------------------------------------ probes (nondet only)

struct Probe {
    down: bool,
    reason: String,
}

fn probe(check: &str, url: &str, expect: &str, max_lag_s: i64) -> Probe {
    let down = |reason: String| Probe { down: true, reason };
    match check {
        "http" => match http_get(url, &[("User-Agent", UA)]) {
            Err(e) => down(format!("unreachable: {e}")),
            Ok(r) if !(200..400).contains(&r.status) => down(format!("HTTP {}", r.status)),
            Ok(r) => {
                if !expect.is_empty() && !String::from_utf8_lossy(&r.body).contains(expect) {
                    down(format!("HTTP {} but body lacks the expected text", r.status))
                } else {
                    Probe { down: false, reason: format!("HTTP {}", r.status) }
                }
            }
        },
        "evm-rpc" => {
            let body = br#"{"jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber","params":["latest",false]}"#;
            match http_post(url, &[("User-Agent", UA), ("Content-Type", "application/json")], body) {
                Err(e) => down(format!("unreachable: {e}")),
                Ok(r) if r.status != 200 => down(format!("HTTP {}", r.status)),
                Ok(r) => {
                    let Ok(j) = serde_json::from_slice::<serde_json::Value>(&r.body) else {
                        return down("not JSON-RPC".into());
                    };
                    if json_path(&j, "error").is_some() {
                        return down("JSON-RPC error".into());
                    }
                    let ts = json_path(&j, "result.timestamp")
                        .and_then(|v| v.as_str())
                        .and_then(|h| i64::from_str_radix(h.trim_start_matches("0x"), 16).ok());
                    let Some(ts) = ts else { return down("no head block".into()) };
                    let now = match wall_clock() {
                        Ok(t) => t,
                        Err(e) => return down(format!("clock: {e}")),
                    };
                    let lag = now - ts;
                    if lag > max_lag_s {
                        down(format!("head block is {lag}s old (max {max_lag_s}s)"))
                    } else {
                        Probe { down: false, reason: format!("head block {lag}s old") }
                    }
                }
            }
        }
        _ => down("unknown check".into()),
    }
}

// ------------------------------------------------------------------ contract

fn get(id: u64) -> R<Value> {
    rec_get(&state().slas(), &sid(id)).ok_or_else(|| format!("no SLA #{id}"))
}

fn put(id: u64, v: &Value) {
    rec_put(&state().slas(), &sid(id), v);
}

fn log_event(m: &MessageData, ev: &str, id: u64, extra: Value) {
    let rec = with(&with(&with(&extra, "event", ev.into()), "id", Value::from(id)), "at", Value::from(tx_time(m)));
    state().log().append_slot().store(&calldata::encode(&rec));
}

fn bond(s: &Value) -> R<U256> {
    u256_from_bigint(&f_num(s, "bond"))
}

fn credit(s: &Value) -> R<U256> {
    u256_from_bigint(&f_num(s, "credit"))
}

fn same(a: Option<Address>, b: &Address) -> bool {
    a.map(|x| x.raw()) == Some(b.raw())
}

fn view(s: &Value, now: i64) -> Value {
    let next = num_i64(s, "last_incident_at") + num_i64(s, "cooldown_s");
    let mut v = with(s, "next_report_at", Value::from(if num_i64(s, "last_incident_at") == 0 { 0 } else { next }));
    if f_str(s, "status") == "active" && now > num_i64(s, "term_end") {
        v = with(&v, "expired", Value::Bool(true));
    }
    v
}

#[derive(Default)]
pub struct SlaEscrow;

impl SlaEscrow {
    fn run_probe(&self, id: u64, s: &Value) -> R<Value> {
        run_nondet(&map([
            ("op", "probe".into()),
            ("id", Value::from(id)),
            ("check", Value::Str(f_str(s, "check"))),
            ("url", Value::Str(f_str(s, "url"))),
            ("expect", Value::Str(f_str(s, "expect"))),
            ("max_lag_s", Value::from(num_i64(s, "max_lag_s"))),
        ]))
    }

    fn create(&self, m: &MessageData, args: &[Value]) -> R<Value> {
        let bond = u256_from_bigint(&m.value)?;
        let name: String = arg_str(args, 0, "name")?.chars().take(60).collect();
        let customer = arg_address(args, 1, "customer")?;
        let url = arg_str(args, 2, "url")?;
        let check = arg_str(args, 3, "check")?;
        let expect: String = arg_str(args, 4, "expect")?.chars().take(80).collect();
        let max_lag_s = arg_u64(args, 5, "max_lag_s")?;
        let credit = u256_from_bigint(&arg_int(args, 6, "credit_wei")?)?;
        let cooldown_s = arg_u64(args, 7, "cooldown_s")?;
        let days = arg_u64(args, 8, "days")?;
        if name.trim().is_empty() {
            return Err("name is required".into());
        }
        if !url.starts_with("https://") || url.len() > 300 {
            return Err("url must be https:// and at most 300 chars".into());
        }
        if check != "http" && check != "evm-rpc" {
            return Err("check must be `http` or `evm-rpc`".into());
        }
        if check == "evm-rpc" && !(30..=86_400).contains(&max_lag_s) {
            return Err("max_lag_s must be 30-86400 for evm-rpc".into());
        }
        if credit.is_zero() || bond < credit {
            return Err("attach a bond of at least one credit".into());
        }
        if !(MIN_COOLDOWN..=MAX_COOLDOWN).contains(&cooldown_s) || !(1..=365).contains(&days) {
            return Err("cooldown_s must be 600-604800 and days 1-365".into());
        }
        if customer.raw() == m.sender_address.raw() {
            return Err("provider and customer must differ".into());
        }
        let now = tx_time(m);
        let mut s = map([
            ("name", name.into()),
            ("provider", Value::Address(m.sender_address)),
            ("customer", Value::Address(customer)),
            ("url", url.into()),
            ("check", check.into()),
            ("expect", expect.into()),
            ("max_lag_s", Value::from(max_lag_s)),
            ("credit", Value::from(credit)),
            ("cooldown_s", Value::from(cooldown_s)),
            ("bond", Value::from(bond)),
            ("created_at", Value::from(now)),
            ("term_end", Value::from(now + days as i64 * 86_400)),
            ("status", "active".into()),
            ("incidents", Value::from(0u64)),
            ("probes", Value::from(1u64)),
            ("last_incident_at", Value::from(0u64)),
        ]);
        // An SLA can only start on an endpoint that validators currently see as up.
        let p = self.run_probe(0, &s)?;
        if f_bool(&p, "down") {
            return Err(format!("endpoint is down right now ({}); an SLA cannot start on it", f_str(&p, "reason")));
        }
        let st = state();
        let id = st.count().get() as u64 + 1;
        st.count().set(id as u32);
        s = with(&s, "id", Value::from(id));
        s = with(&s, "last_probe", map([("down", false.into()), ("reason", Value::Str(f_str(&p, "reason"))), ("at", Value::from(now))]));
        put(id, &s);
        st.bonded().set(st.bonded().get() + bond);
        log_event(m, "created", id, map([("bond", Value::from(bond))]));
        Ok(view(&s, now))
    }

    fn report(&self, m: &MessageData, args: &[Value]) -> R<Value> {
        no_value(m)?;
        let id = arg_u64(args, 0, "id")?;
        let s = get(id)?;
        let now = tx_time(m);
        // The credit is pushed as an external message, which needs a message-fee
        // budget on this transaction. Only the customer may report, so nobody
        // else can burn the cooldown with a budget-less report.
        if !same(f_addr(&s, "customer"), &m.sender_address) {
            return Err("only the SLA's customer can report an outage".into());
        }
        if f_str(&s, "status") != "active" {
            return Err(format!("SLA #{id} is {}", f_str(&s, "status")));
        }
        if now > num_i64(&s, "term_end") {
            return Err(format!("SLA #{id} has ended"));
        }
        let last = num_i64(&s, "last_incident_at");
        if last != 0 && now < last + num_i64(&s, "cooldown_s") {
            return Err(format!("cooldown: next report accepted at {}", last + num_i64(&s, "cooldown_s")));
        }
        let credit = credit(&s)?;
        if bond(&s)? < credit {
            return Err("bond is exhausted".into());
        }
        let p = self.run_probe(id, &s)?;
        let down = f_bool(&p, "down");
        let reason = f_str(&p, "reason");
        let mut s = with(&s, "probes", Value::from(f_u64(&s, "probes") + 1));
        s = with(&s, "last_probe", map([("down", down.into()), ("reason", Value::Str(reason.clone())), ("at", Value::from(now))]));
        if down {
            let left = bond(&s)? - credit;
            s = with(&s, "bond", Value::from(left));
            s = with(&s, "incidents", Value::from(f_u64(&s, "incidents") + 1));
            s = with(&s, "last_incident_at", Value::from(now));
            put(id, &s);
            let st = state();
            st.bonded().set(st.bonded().get().saturating_sub(credit));
            st.credited().set(st.credited().get() + credit);
            let customer = f_addr(&s, "customer").ok_or("corrupt SLA")?;
            transfer_to_eoa(customer, credit)?;
            log_event(m, "incident", id, map([("credit", Value::from(credit)), ("reason", reason.into()), ("reporter", Value::Address(m.sender_address))]));
        } else {
            put(id, &s);
            log_event(m, "probe_ok", id, map([("reason", reason.into()), ("reporter", Value::Address(m.sender_address))]));
        }
        Ok(view(&s, now))
    }

    fn top_up(&self, m: &MessageData, args: &[Value]) -> R<Value> {
        let v = u256_from_bigint(&m.value)?;
        if v.is_zero() {
            return Err("attach GEN to top up".into());
        }
        let id = arg_u64(args, 0, "id")?;
        let s = get(id)?;
        if f_str(&s, "status") != "active" {
            return Err(format!("SLA #{id} is {}", f_str(&s, "status")));
        }
        let s = with(&s, "bond", Value::from(bond(&s)? + v));
        put(id, &s);
        let st = state();
        st.bonded().set(st.bonded().get() + v);
        log_event(m, "topped_up", id, map([("amount", Value::from(v))]));
        Ok(view(&s, tx_time(m)))
    }

    /// The customer ends the SLA early and waives further credits.
    fn release(&self, m: &MessageData, args: &[Value]) -> R<Value> {
        no_value(m)?;
        let id = arg_u64(args, 0, "id")?;
        let s = get(id)?;
        if f_str(&s, "status") != "active" {
            return Err(format!("SLA #{id} is {}", f_str(&s, "status")));
        }
        if !same(f_addr(&s, "customer"), &m.sender_address) {
            return Err("only the customer can release an SLA early".into());
        }
        let now = tx_time(m);
        let s = with(&with(&s, "status", "released".into()), "released_at", Value::from(now));
        put(id, &s);
        log_event(m, "released", id, map([]));
        Ok(view(&s, now))
    }

    /// The provider takes back the remaining bond once the term is over or the
    /// customer released the SLA. Pull-based: the provider triggers its own transfer.
    fn withdraw(&self, m: &MessageData, args: &[Value]) -> R<Value> {
        no_value(m)?;
        let id = arg_u64(args, 0, "id")?;
        let s = get(id)?;
        let now = tx_time(m);
        let status = f_str(&s, "status");
        if !same(f_addr(&s, "provider"), &m.sender_address) {
            return Err("only the provider can withdraw the bond".into());
        }
        if !(status == "released" || (status == "active" && now > num_i64(&s, "term_end"))) {
            return Err("the bond is locked until the term ends or the customer releases the SLA".into());
        }
        let rest = bond(&s)?;
        let s = with(&with(&with(&s, "status", "closed".into()), "bond", Value::from(0u64)), "closed_at", Value::from(now));
        put(id, &s);
        let st = state();
        st.bonded().set(st.bonded().get().saturating_sub(rest));
        transfer_to_eoa(m.sender_address, rest)?;
        log_event(m, "withdrawn", id, map([("amount", Value::from(rest))]));
        Ok(view(&s, now))
    }
}

impl ContractExt for SlaEscrow {
    type Error = String;

    fn handle_deploy(&mut self, _m: MessageData, _a: Vec<Value>, _k: calldata::Map) -> R<Value> {
        Ok(Value::Null)
    }

    fn handle_method(&mut self, m: MessageData, method: String, args: Vec<Value>, _k: calldata::Map) -> R<Value> {
        let now = tx_time(&m);
        match method.as_str() {
            "create" => self.create(&m, &args),
            "report" => self.report(&m, &args),
            "top_up" => self.top_up(&m, &args),
            "release" => self.release(&m, &args),
            "withdraw" => self.withdraw(&m, &args),
            "get" => Ok(view(&get(arg_u64(&args, 0, "id")?)?, now)),
            "list" => {
                let st = state();
                let n = st.count().get() as u64;
                let mut out = Vec::new();
                let mut i = n;
                while i >= 1 && out.len() < 100 {
                    if let Some(s) = rec_get(&st.slas(), &sid(i)) {
                        out.push(view(&s, now));
                    }
                    i -= 1;
                }
                Ok(map([("total", Value::from(n)), ("items", Value::Array(out))]))
            }
            "events" => {
                let filter = args.first().and_then(|v| v.as_number()).and_then(|b| u64::try_from(b.clone()).ok());
                let log = state().log();
                let mut out = Vec::new();
                let mut i = log.len();
                while i > 0 && out.len() < 50 {
                    i -= 1;
                    if let Ok(v) = calldata::decode(&log.index(i).load()) {
                        if filter.map(|id| f_u64(&v, "id") == id).unwrap_or(true) {
                            out.push(v);
                        }
                    }
                }
                Ok(Value::Array(out))
            }
            "stats" => {
                let st = state();
                Ok(map([
                    ("slas", Value::from(st.count().get())),
                    ("bonded", Value::from(st.bonded().get())),
                    ("credited", Value::from(st.credited().get())),
                    ("balance", Value::from(self_balance()?)),
                ]))
            }
            "version" => Ok("sla-escrow 1.0.0".into()),
            _ => Err(format!("unknown method: {method}")),
        }
    }
}

impl Contract for SlaEscrow {
    fn handle_main(&mut self, message: MessageData, data: bytes::Bytes) -> Result<Value, String> {
        ContractExt::handle_main(self, message, data.to_vec())
    }

    fn handle_sandbox(&mut self, _m: MessageData, _d: bytes::Bytes) -> Result<Vec<u8>, String> {
        Err("sandbox not supported".into())
    }

    fn handle_consensus_stage(&mut self, _m: MessageData, data: bytes::Bytes, stage: ConsensusStageData) -> Result<Value, String> {
        let req = calldata::decode(&data).map_err(|e| format!("{e:?}"))?;
        if f_str(&req, "op") != "probe" {
            return Err("unknown nondet op".into());
        }
        let p = probe(&f_str(&req, "check"), &f_str(&req, "url"), &f_str(&req, "expect"), num_i64(&req, "max_lag_s"));
        match Stage::from(stage) {
            Stage::Leader => Ok(map([("down", Value::Bool(p.down)), ("reason", Value::Str(p.reason))])),
            // The verdict must match; the reason text (exact lag, error wording) may differ per node.
            Stage::Validator(lr) => Ok(Value::Bool(
                leader_value(&lr).and_then(|l| f(l, "down")).and_then(|d| d.as_bool()) == Some(p.down),
            )),
        }
    }
}

glkit::genlayer_sdk::contract_main!(SlaEscrow);
