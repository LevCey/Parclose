# Changelog

Changelog for `contracts`.

## [0.1.0] - 2026-06-05
### Added
- `FundToken` — transfer-restricted CEP-18 token (whitelist hook on transfer/transfer_from).
- `CashToken` — freely transferable CEP-18 test token for the cash leg.
- `WindowRegistry` — crossing-window lifecycle and published crossing rule with version history.
- `SealedOrderBook` — sealed-order intake (ciphertext only) with a per-window ordered hash-chain commitment.
