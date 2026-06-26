# Parclose

**Private, fair redemption matching for tokenized real-world-asset (RWA) funds — coordinated by autonomous liquidity agents and settled on Casper.**

Parclose runs a confidential **crossing window**: investors and autonomous agents submit sealed orders, a confidential-compute enclave computes one fair clearing price without revealing anyone's order, and a Casper smart contract verifies that result and settles it on-chain. Built for the **Casper Agentic Buildathon 2026** (Innovation Track).

> Status: active development · Casper **Testnet** prototype · custodies no real value. See [Status](#status).

---

## The problem

Semi-liquid tokenized RWA funds — private credit, real-estate income, revenue-backed vehicles — have no fair way to clear redemptions:

- **Gating is blunt and blind.** When redemptions spike, managers freeze withdrawals because there is no continuous, fair price-discovery mechanism to match exiting and entering capital. Investors are stuck, or exit at arbitrary haircuts.
- **Order flow leaks.** Any on-chain or intermediated matching exposes who wants out, how much, and at what price — inviting front-running of the redemption queue and penalizing the very investors a fund should protect.
- **Marks are stale.** NAV is typically lagged and periodic, so redemptions clear at a price disconnected from reality.

**The thesis:** a fund does not need a deep public order book to clear redemptions fairly. It needs only its own exiting and entering participants, a fair clearing rule, and confidentiality.

*(Supporting market figures — market size, discounts to NAV, and redemption pressure — are third-party context and will be cited before submission.)*

---

## What Parclose does

A periodic confidential crossing window with four moving parts:

1. **Confidential order submission.** Redeemers and subscribers — directly, or via an agent — submit sealed orders (side, size, limit price) as encrypted inputs. Before clearing, only ciphertext is posted on-chain; the plaintext is readable only inside the enclave.
2. **Autonomous liquidity agents.** Two or more autonomous liquidity agents act as intelligent, confidential counterparties. Each perceives attested market data and fund state, reasons about a pricing and sizing strategy under uncertainty *and* competition, and submits its own sealed order — unable to see any rival's order or strategy.
3. **Fair clearing inside the enclave.** The enclave computes a single **uniform clearing price** and matched set under a published rule, and signs an attestation over the computation.
4. **Verifiable on-chain settlement.** A Casper smart contract verifies the enclave's attestation — signature, code measurement, and freshness — then atomically settles the matched redemptions and subscriptions into a compliant, transfer-restricted fund token.

The result is confidential price discovery, fair sealed competition, and a settlement anyone can verify was produced by the agreed rule — without leaking any participant's pre-clearing intent.

---

## The agents are the protagonists

Parclose's liquidity agents are genuine autonomous agents, not a pricing formula with agent branding. Each runs a real **perceive → reason → act** loop:

- **Perceive** — read fund state, prior clearing prices, attested market/NAV inputs, and the agent's own inventory and risk limits.
- **Reason** — an LLM strategy layer forms a view under uncertainty and competition, weighing the attested signal, its inventory and risk limits, fill probability against rivals, and prior clearing context. It produces a short, human-readable rationale and a concrete order.
- **Act** — sign and submit a sealed order through a scoped smart account, then settle on-chain after clearing.

At least two agents run independently, with different strategies, blind to one another — converging into one fair clearing price. Each agent's reasoning is legible (surfaced off-chain in the dashboard), and its order shifts meaningfully when its inputs change: it reasons, rather than applying a fixed closed-form rule.

---

## How it works

```
 Participants / agents ──► sealed order = ciphertext ──► SealedOrderBook (Casper)
   (LLM + scoped smart account)                          stores ciphertext only; no plaintext
        │ perceive market / fund state                            │
        ▼                                                          ▼
   reason (LLM) ─► sealed order                       confidential enclave
                                                       uniform clearing price + matched set
                                                       + signed, domain-separated attestation
                                                                   │
                                                                   ▼
                                            CrossingEngine (Casper)
                                            verify attestation (signature + code measurement
                                              + freshness + domain binding), then settle
                                              atomically from pre-funded escrow,
                                              between whitelisted holders
                                            ├─ FundToken   compliant, transfer-restricted
                                            ├─ CashToken   test cash leg (no value)
                                            └─ WindowRegistry  windows + published rule/version
                                                                   │ events
                                                                   ▼
                                            streaming dashboard (the live demo)
```

**Contracts** (Rust / Odra on Casper Testnet):

- `SealedOrderBook` — sealed-order intake; stores ciphertext only, never plaintext order fields, and commits to the exact set of orders the enclave clears.
- `CrossingEngine` — verifies the enclave attestation and atomically settles the matched set from pre-funded escrow.
- `FundToken` — a compliant, transfer-restricted token (a stand-in for an ERC-3643-style security token); transfers occur only between whitelisted holders.
- `CashToken` — a valueless test token used as the cash leg of settlement.
- `WindowRegistry` — opens and closes crossing windows and publishes the crossing rule and its version history.

**Enclave** — a confidential-compute guest that ingests the sealed orders, computes a deterministic uniform-price crossing, and produces a signed, domain-separated attestation. During development the flow runs against a clearly-labeled **testnet/dev attestation signer (not a hardware TEE)** so the system works end to end; the production target is a real TEE attestation path, swapped in behind the identical claim structure.

---

## What is private, and what is public

- **Private (never on-chain):** individual submitted orders — side, size, limit price — the full order book, and each agent's strategy and rationale. Before clearing, only ciphertext appears on-chain. This is the front-running protection. Agent rationales shown in the dashboard are off-chain demo artifacts — not posted on-chain, not included in settlement payloads, and not visible to rival agents before the window clears.
- **Public (on-chain):** the final attested clearing price and the settlement transfers (final fills) required to execute the match.
- **Out of scope (for now):** hiding the final fills themselves. This prototype keeps pre-clearing intent confidential and settles transparently; confidential settlement is future work.

---

## Why Casper

Parclose uses Casper as a coherent, auditor-legible home for confidential, regulated settlement — **not** a claim that this is impossible elsewhere; the confidential-compute layer is portable. What Casper provides here:

- a live, on-chain-verifiable attestation pattern to build the verification step against;
- compliant, transfer-restricted settlement aligned with regulated-RWA use;
- account and contract semantics suited to institutional use — multi-party authorization, transparently upgradeable contracts with an on-chain change history, and fast deterministic finality for atomic settlement.

---

## Repository layout

```
contracts/   Odra/Rust smart contracts: SealedOrderBook, CrossingEngine, FundToken, CashToken, WindowRegistry
enclave/     confidential clearing (uniform-price crossing), sealed-order decryption, and a labeled testnet/dev attestation signer
seal/        sealed-order encryption (X25519 ECDH + XChaCha20-Poly1305) binding each order to its window and submitter
agents/      autonomous liquidity agents (perceive → reason → act): the LLM strategy and a blind-competition harness
shared/      canonical cross-component encodings (order, attestation claim, clearing result)
dashboard/   streaming demo UI (planned)
```

---

## Status

Parclose is in **active development** for the Casper Agentic Buildathon 2026 (Qualification Round, June 2026). It is a Casper **Testnet** prototype: not production software, not audited, and it custodies no real value — both settlement legs are test tokens.

**Implemented and tested off-chain so far:** the five smart contracts (attestation verification + escrow settlement, the compliant fund token and cash leg, sealed-order intake, and the window registry); the confidential uniform-price clearing rule and sealed-order decryption; the sealed-order encryption; the labeled testnet/dev attestation signer; and the autonomous liquidity agents (reasoning and blind competition). All are covered by an automated test suite, and the full off-chain flow — seal → confidential clearing → signed attestation → on-chain verification + atomic settlement — runs end to end against the contracts in Odra's test VM.

Toward the live prototype:

- [x] Smart contracts deployed on Casper Testnet
- [ ] Confidential clearing on a real TEE (a labeled testnet/dev attestation signer is used during development)
- [ ] Autonomous liquidity agents driving live windows (two or more, competing blind)
- [ ] Streaming demo dashboard
- [ ] Demo video

### Deployed on Casper Testnet

The five contracts are live on Casper Testnet (`casper-test`), wired together (registry ↔ engine) and with the custody endpoint whitelisted. Contract package hashes (view on the [block explorer](https://testnet.cspr.live)):

| Contract | Package hash |
|---|---|
| `WindowRegistry` | [`66f68780…e7190`](https://testnet.cspr.live/contract-package/66f68780c36d3646415170125503198128965e369e0132719f42af26bece7190) |
| `FundToken` | [`4922ed8a…de39e`](https://testnet.cspr.live/contract-package/4922ed8af46bb36a5d5ab3507107c86d775e535ee58e9bd69ca25097024de39e) |
| `CashToken` | [`0c9507ca…f01eb`](https://testnet.cspr.live/contract-package/0c9507ca709d750f99fcd4b9c69eddd93598f6323a9b2c73f28e5590d64f01eb) |
| `SealedOrderBook` | [`2895f385…42a9d`](https://testnet.cspr.live/contract-package/2895f3852fc8e070ff1b7fa74ededd46587c1d7e43badcd51b965d0a93b42a9d) |
| `CrossingEngine` | [`ead50d46…c9150`](https://testnet.cspr.live/contract-package/ead50d4643379c2b7d82f872d59449164501de13d8d0d42f35d0cd5dc93c9150) |

The `CrossingEngine` is configured with the dev attestation signer's secp256k1 key as its enclave trust root, so a signed clearing result verifies and settles on-chain.

A full crossing window has been run end to end on Testnet — open → escrow both legs → submit sealed orders (ciphertext only) → close → off-chain clearing → signed attestation → on-chain verification + atomic settlement → withdraw. The settlement transaction (the `CrossingEngine` verifying the attestation and settling from escrow): [`894d6dfb…79f69e`](https://testnet.cspr.live/transaction/894d6dfbb4096d42739ddfca83b0f4b1235b6215fe7c41d6685c62178179f69e).

---

## Development

Parclose is Rust with the [Odra](https://odra.dev) framework (compiled to Wasm) for the contracts, a confidential-compute guest for the enclave, and an LLM-driven agent runtime. The streaming dashboard is planned.

### Prerequisites

- **Rust** via [rustup](https://rustup.rs). Each crate pins its toolchain in a `rust-toolchain` file; rustup installs the pinned toolchain automatically on first build.
- **[cargo-odra](https://github.com/odradev/cargo-odra)** for the contracts: `cargo install cargo-odra`.
- **curl** — used by the agents' reasoning client for the live (model-backed) path.

### Build & test

Each component is its own crate; run its test suite from the crate directory:

```bash
cd contracts && cargo odra test   # smart contracts (Odra VM)
cd enclave   && cargo test        # confidential clearing, sealed-order decryption, dev signer
cd seal      && cargo test        # sealed-order encryption
cd agents    && cargo test        # liquidity agents: reasoning, blind competition, encryption client
```

### Run the demos (offline, no network)

```bash
# Two liquidity agents reason blind and converge into one uniform clearing price
cd agents && cargo run --example compete

# The off-chain pipeline end to end: seal → open → clear → sign an attestation
cd enclave && cargo run --example dev_signer
```

To run the agents against the real model, set an API key and use the live example (the only path that makes a network call); without a key the agents use the offline reasoning stand-in:

```bash
cd agents
export ANTHROPIC_API_KEY=sk-...        # optionally ANTHROPIC_MODEL
cargo run --example live_compete
```

### Configuration

All endpoints, keys, and contract addresses are supplied via environment variables — nothing is hard-coded or committed. See [`.env.example`](.env.example) for the full list; copy it to `.env` and fill in your own values.

### Deploy (Casper Testnet)

The contracts are deployed with the Odra livenet deploy binary. Build the wasm first (`cargo odra build`, then `wasm-opt` each artifact), then:

```bash
cd contracts
ODRA_CASPER_LIVENET_NODE_ADDRESS=http://<node>:7777 \
ODRA_CASPER_LIVENET_EVENTS_URL=http://<node>:9999/events \
ODRA_CASPER_LIVENET_CHAIN_NAME=casper-test \
ODRA_CASPER_LIVENET_SECRET_KEY_PATH=/path/to/secret_key.pem \
cargo run --bin deploy --features livenet
```

It deploys all five contracts, wires the registry to the engine, whitelists the custody endpoint, and prints every address (see [Status](#status) for the live ones). Note: keep each deploy's gas under the network `block_gas_limit`, and set the transaction's gas-price tolerance to the network's current gas price — a higher tolerance is rejected as an "invalid pricing mode" on the current Testnet.

---

## Roadmap

The prototype focuses on the core end-to-end flow above. Beyond it:

- a third competing agent;
- an optional fairness attestation (the enclave additionally attesting that the crossing was uniform-price and non-preferential);
- optional per-window settlement fees (a possible future use of x402 — not used in production today);
- support for multiple funds;
- confidential settlement (hiding final fills).

---

## Disclaimer

Testnet prototype built for a hackathon: no real value, not audited, not production software. The testnet/dev attestation signer used during development is a stand-in and does not provide hardware-TEE guarantees. Any market figures referenced are third-party reporting cited for context.

---

License: [Apache-2.0](LICENSE)
