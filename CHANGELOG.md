# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — 2026-09-29

### Added

- `route <ticker>` — deterministic-first shard resolution: static family
  table (zero API calls) → live `exchange_index` read (the only VERIFIED
  truth). Gate exit codes: `0` verified, `3` gated hint, `4` unknown.
- `classify <text>` — semantic shard hint via Jev (TypeSafe System One),
  confidence-gated at 0.80. A hint is for fund pre-positioning only and can
  never promote itself to VERIFIED.
- `balance` — per-shard cash in exact integer centicents.
- `fees <series> [price_cc]` — exact taker/maker fee schedules read live
  from the series endpoint, with integer fee math validated against a real
  fill (84cc taker at 86c, to the cent) and the published fee table.
- `prefund <ticker> $N [--wait]` — moves the funding shortfall from the
  richest shard and waits until the transfer is OBSERVED landed before
  releasing the order pipeline (transfers are async; ordering against
  unlanded cash is the cash-clamp bug class). Typed `Insufficient` with
  exit code 5; never a silent partial transfer.
- `ledger` — append-only JSONL transfer ledger, idempotent on
  `transfer_id`, torn-write tolerant, with per-shard live reconciliation
  that labels drift honestly (fills/settlements/fees).
- `exposure` — positions + resting orders aggregated per shard, static
  route first with one bounded live read per unknown line.
- `sim <seed>` — prefund-wait state machine, replayable by seed.
- `sim-world <seed>` / `sim-world clamp` — multi-agent DST world simulator
  with cash-conservation, observed-cash-only fills, and bounded tick budget
  invariants checked after every transition.

[0.1.0]: https://github.com/copyleftdev/shard-router/releases/tag/v0.1.0
