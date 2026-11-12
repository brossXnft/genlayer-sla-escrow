# SLA Escrow

Uptime guarantees backed by GEN and judged by independent validators. A provider bonds GEN for one customer and one endpoint. When the customer reports an outage, the leader and every validator probe the endpoint from their own network, and the agreed credit is paid only if they agree it is down. The contract is written in Rust on GenLayer.

- **App:** https://brossxnft.github.io/genlayer-sla-escrow/
- **Contract (Bradbury testnet):** `0x762647adc6541a67251e3a8883ca6d89a7c76156` ([explorer](https://scan.bradbury.genlayer.com/address/0x762647adc6541a67251e3a8883ca6d89a7c76156))
- **Language:** Rust, `genlayer_sdk` (genvm-executor `v0.3-dev` @ `6ec6c39`), deployed with [genlayer-chunked-deploy](https://github.com/brossXnft/genlayer-chunked-deploy)

## The problem

Service-level agreements for APIs and RPC endpoints promise credits for downtime, but collecting them works against the customer. You open a ticket, the provider checks its own status page, which it writes itself, and decides whether the outage counts. Crypto infrastructure makes this worse: a dApp loses users when its RPC provider stalls, and has no neutral record of the outage. The real question, "is this endpoint down right now?", can be answered by several independent observers. A GenLayer validator set is exactly that.

## What it does

1. **Create (provider).** `create(name, customer, url, check, expect, max_lag_s, credit_wei, cooldown_s, days)` with the bond attached. Validators probe the endpoint first; an SLA cannot start on an endpoint that is already down.
2. **Report (customer).** When the customer's monitoring sees an outage, the customer calls `report(id)`, by hand in the app or from a monitoring bot.
3. **Probe (consensus).** The leader and each validator run the check from their own network:
   - `http`: `GET url`. Down on a connection error, a timeout, or a status outside 200–399, or when `expect` is set and the body does not contain it.
   - `evm-rpc`: `POST eth_getBlockByNumber("latest")`. Down on any error, or when the head block is older than `max_lag_s` by the node's own clock. This catches a node that still answers but has stopped syncing.

   Validators vote *agree* when their verdict (down or up) equals the leader's. The reason text, such as the exact lag or error message, may differ and is not compared.
4. **Credit.** If consensus says down, `credit` moves from the bond to the customer, at most once per `cooldown_s`. If consensus says up, the probe is recorded and nothing is paid. The reporter paid the fee, which discourages spam.
5. **End.** After the term, or after the customer calls `release(id)` early, the provider calls `withdraw(id)` to take the remaining bond.

### Trust model

| Question | Answer |
|---|---|
| Who decides there was an outage? | The validator set, each measuring independently at report time. Neither party's own monitoring or status page is trusted. |
| What if the endpoint flaps? | Validators probe seconds apart. If they disagree, the transaction does not reach consensus and nothing is paid: the provider gets the benefit of the doubt, and only clear outages cost money. |
| Can one region's blip trigger a payout? | Only if the leader and the majority of validators, on different networks, all see it. |
| Can the provider dodge paying? | The bond is locked in the contract until the term ends or the customer releases it. |
| Can a third party grief it? | Only the customer can report, and only the provider can withdraw. GEN leaves the contract as an external message that GenLayer executes only when the transaction carries a message-fee budget; we measured on Bradbury that without one the call succeeds but the transfer never happens. So each transfer is triggered by its own recipient, and the app always attaches a budget (0.01 GEN, mostly refunded). |

## The app

Plain HTML and ES modules with no build step. Roles are detected from the connected wallet:

- **Check now** (anyone): simulates the customer's `report` on one node with `gen_call`. The endpoint gets probed and the likely verdict appears without a transaction or fee.
- **Customer:** Report outage, Release early.
- **Provider:** create an SLA, top up the bond, withdraw after the term.
- **History:** the contract's event log for the SLA (created, probe_ok, incident, topped_up, released, withdrawn).
- Every write is simulated first, signed as `addTransaction` on ConsensusMain, and followed through the full lifecycle: leader execution, commit, reveal, accepted, finalized, with validator votes.

Run it locally with any static server: `python -m http.server` in the repository root.

## Contract methods

| Method | Kind | Who |
|---|---|---|
| `create(...)` | write, payable, nondet | provider; cooldown 600 s to 7 days, term 1 to 365 days, bond ≥ one credit, provider ≠ customer |
| `report(id)` | write, nondet | customer; needs a message-fee budget |
| `top_up(id)` | write, payable | anyone |
| `release(id)` | write | customer |
| `withdraw(id)` | write | provider, after the term or release; needs a message-fee budget |
| `get(id)`, `list()`, `events(id?)`, `stats()` | view | anyone |

## Deployment

| | |
|---|---|
| Network | GenLayer Bradbury testnet (chain 4221) |
| Contract | [`0x762647adc6541a67251e3a8883ca6d89a7c76156`](https://scan.bradbury.genlayer.com/address/0x762647adc6541a67251e3a8883ca6d89a7c76156) |
| Code | `dist/sla-escrow.wasm`, 258,843 bytes, SHA-256 `3a62f6e8f852a93cf609642ed52dba2fa3bcaa82da0f72947e35be44b08ea55d` |
| Activation tx | [`0x1e80b46021853ff7…`](https://scan.bradbury.genlayer.com/tx/0x1e80b46021853ff7fdea9a62524cd94cf4f607c8fd0c6b4b7f115b8c2a65d262) |

`tools/build.sh` rebuilds the same bytes (Rust 1.98.1, binaryen version_133) so the SHA-256 can be compared with the code the chain runs, for example with the verifier at https://brossxnft.github.io/genlayer-chunked-deploy/.

Transactions that exercise the contract on Bradbury (all finalized with 5 validators unless noted):

| What | GenLayer tx |
|---|---|
| SLA #1 created on an HTTP endpoint (GitHub contents API for `demo/heartbeat.txt`); validators probed it up first | [`0xdc05ab94ee23eb03…`](https://scan.bradbury.genlayer.com/tx/0xdc05ab94ee23eb03ddedbcb2ae3ee6981d2c7948372b39dfe815fcddca700966) |
| SLA #2 created on the GenLayer Chain RPC (`evm-rpc`, head block at most 120 s old) | [`0x3468bcae7ef5a84b…`](https://scan.bradbury.genlayer.com/tx/0x3468bcae7ef5a84bbf0cf073964cd187380a3e690728472823d53faf7874b348) |
| Customer reports SLA #2: validators see the head block 2 s old, so it is up and nothing is paid | [`0xad6d1471c34da3e4…`](https://scan.bradbury.genlayer.com/tx/0xad6d1471c34da3e4d3efd0d9d7e53acf4754ac4f2129655c8358555afd2c47e3) |
| `demo/heartbeat.txt` deleted, endpoint answers 404; customer reports SLA #1: validators agree it is down, 0.2 GEN credit sent to the customer at finalization (contract balance 0.8 → 0.6 GEN) | [`0x4da5f6d0ab1ec402…`](https://scan.bradbury.genlayer.com/tx/0x4da5f6d0ab1ec402c22185a9387b453c74ad56f7e0b6d6ce4fc3b64474e5c3ad) |

Provider `0xaa2160917de3e87991fed9fe5a5b56c202dbf4b0`, customer `0x51f21e5c034b06824d95008582b78b948f6f812c` (both testnet wallets of the author).

## Build and deploy

```bash
cargo build --release --target wasm32-wasip1
wasm-opt -Oz --enable-bulk-memory --enable-sign-ext --enable-mutable-globals \
  --enable-nontrapping-float-to-int target/wasm32-wasip1/release/sla-escrow.wasm -o sla-escrow.wasm
python tools/wasm_strip.py sla-escrow.wasm sla-escrow.wasm --version v0.3.0
python tools/chunked_deploy.py sla-escrow.wasm --test-method version --send
node --test web/test/*.test.mjs
```

Then put the address in `web/config.js`.

## Limits and next steps

- A probe is a single moment. Outages shorter than the time between the customer's monitoring alert and the transaction are not captured.
- Next: scheduled probes paid from a prepaid budget (messages from the contract to itself), proportional credits per minute of downtime, and checks for gRPC and WebSocket endpoints.

## License

MIT
