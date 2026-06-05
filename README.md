# Aperture

**Private, fair redemption matching for tokenized real-world-asset (RWA) funds — coordinated by autonomous liquidity agents and settled on Casper.**

Aperture runs a confidential **crossing window**: investors and autonomous agents submit sealed orders, a confidential-compute enclave computes one fair clearing price without revealing anyone's order, and a Casper smart contract verifies that result and settles it on-chain. Built for the **Casper Agentic Buildathon 2026** (Innovation Track).

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

## What Aperture does

A periodic confidential crossing window with four moving parts:

1. **Confidential order submission.** Redeemers and subscribers — directly, or via an agent — submit sealed orders (side, size, limit price) as encrypted inputs. Before clearing, only ciphertext is posted on-chain; the plaintext is readable only inside the enclave.
2. **Autonomous liquidity agents.** Two or more autonomous liquidity agents act as intelligent, confidential counterparties. Each perceives attested market data and fund state, reasons about a pricing and sizing strategy under uncertainty *and* competition, and submits its own sealed order — unable to see any rival's order or strategy.
3. **Fair clearing inside the enclave.** The enclave computes a single **uniform clearing price** and matched set under a published rule, and signs an attestation over the computation.
4. **Verifiable on-chain settlement.** A Casper smart contract verifies the enclave's attestation — signature, code measurement, and freshness — then atomically settles the matched redemptions and subscriptions into a compliant, transfer-restricted fund token.

The result is confidential price discovery, fair sealed competition, and a settlement anyone can verify was produced by the agreed rule — without leaking any participant's pre-clearing intent.

---

## The agents are the protagonists

Aperture's liquidity agents are genuine autonomous agents, not a pricing formula with agent branding. Each runs a real **perceive → reason → act** loop:

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

Aperture uses Casper as a coherent, auditor-legible home for confidential, regulated settlement — **not** a claim that this is impossible elsewhere; the confidential-compute layer is portable. What Casper provides here:

- a live, on-chain-verifiable attestation pattern to build the verification step against;
- compliant, transfer-restricted settlement aligned with regulated-RWA use;
- account and contract semantics suited to institutional use — multi-party authorization, transparently upgradeable contracts with an on-chain change history, and fast deterministic finality for atomic settlement.

---

## Repository layout (planned)

```
contracts/   Odra/Rust smart contracts (intake, verify + settlement, fund token + cash leg, window registry)
enclave/     confidential clearing (uniform-price crossing) + attestation; includes a labeled testnet/dev signer
agents/      autonomous liquidity agents (perceive → reason → act)
dashboard/   streaming demo UI
shared/      canonical encodings shared across the above
```

---

## Status

Aperture is in **active development** for the Casper Agentic Buildathon 2026 (Qualification Round, June 2026). It is a Casper **Testnet** prototype: not production software, not audited, and it custodies no real value — both settlement legs are test tokens.

- [ ] Smart contracts on Casper Testnet
- [ ] Confidential clearing enclave (plus a labeled testnet/dev attestation signer for development)
- [ ] Autonomous liquidity agents (two or more, competing blind)
- [ ] Streaming demo dashboard
- [ ] Demo video

Deployed contract addresses and transaction links will be published here as the prototype lands.

---

## Development

The intended stack is Rust with the Odra framework (compiled to Wasm) for the contracts, a confidential-compute guest for the enclave, an LLM-driven agent runtime, and a streaming dashboard.

**Prerequisites (planned):** Rust and cargo; the Odra toolchain; the Casper client / CLI; and a JavaScript runtime (Node.js) for the dashboard.

**Configuration:** all network endpoints, keys, and contract addresses are supplied via environment variables — nothing is hard-coded or committed; the required variables will be documented in an example env file.

Build, test, deploy (Casper Testnet), and run-demo instructions will be added here as each component lands.

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
