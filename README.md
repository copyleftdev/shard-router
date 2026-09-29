# shard-router

[![CI](https://github.com/copyleftdev/shard-router/actions/workflows/ci.yml/badge.svg)](https://github.com/copyleftdev/shard-router/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Crates.io](https://img.shields.io/badge/roadmap-crates.io-orange)](#roadmap)

Deterministic-first shard router for the [Kalshi](https://kalshi.com) exchange.
Rust, TigerStyle discipline, integer money, pure state machines, DST/VOPR
simulation testing.

> Unofficial tool. Not affiliated with, endorsed by, or supported by Kalshi.

## Why this exists

Kalshi runs **four parallel matching-engine shards** (`exchange_index` 0-3).
Cash is held *per shard* and orders draw only from their market's shard.
Every one of these failure modes is real and was paid for in production
before this tool existed:

| incident | what happened | root cause |
|---|---|---|
| the unit bug | a $15 transfer moved $0.15 | `amount` is in centicents (`150_000`, not `1500`); the unit was assumed, not encoded |
| the zero-balance bounce | an order on a tennis market bounced `insufficient_balance` while the account was flush | all cash sat on a different shard than the market's |
| the cash clamp | a planned 36-contract fill clamped to 20 at execution | a transfer was in flight (async, seconds) when the order fired — cash existed but was not *observed* |

The common cause: **knowledge about a sharded exchange lived in an
operator's head, not in a typed, tested interface.** shard-router makes
that knowledge a program with invariants, so scripts and agents stop
guessing:

```
shard-router --json route  $TICKER          # which shard, VERIFIED
shard-router       prefund $TICKER 15 --wait  # cash is THERE when your order fires
shard-router       fees    $SERIES 5800    # the exact fee constants for your EV math
```

## The tier model — never predict what you can read

| tier | mechanism | cost | authority |
|------|-----------|------|-----------|
| 1 | static table of verified series families | zero | VERIFIED |
| 2 | live exchange read (`GET /markets/{t}` → `exchange_index`) | 1 API call | VERIFIED (only truth) |
| 3 | semantic classification of free text (Jev, TypeSafe System One) | 1 model call | **HINT only** — confidence-gated at 0.80 |

A tier-3 hint exists solely to pre-position funds early and can **never**
promote itself to VERIFIED — order placement always re-reads the shard via
tier 2. Below-gate hints surface as UNKNOWN rather than routing.

## Install

```bash
git clone https://github.com/copyleftdev/shard-router
cd shard-router
cargo build --release
# binary at target/release/shard-router
```

Configuration is environment-only — the tool holds no credentials, has no
credential file paths compiled in, and writes no secrets anywhere:

| variable | purpose |
|---|---|
| `API_KEY_ID` | Kalshi API key id |
| `KALSHI_PRIVATE_KEY` | RSA private key as inline PEM, **or** |
| `KALSHI_PRIVATE_KEY_PATH` | path to the PEM file |
| `JEV_API_KEY` | optional — enables the semantic tier; everything else works without it |
| `SHARD_ROUTER_LEDGER` | optional — override the ledger path (default `$HOME/.local/share/shard-router/ledger.jsonl`) |

## Usage

```
shard-router shards                              # the shard map
shard-router route  KXATPCHALLENGERMATCH-...     # static -> live resolution
shard-router classify "BTC daily range market"   # semantic hint (UNVERIFIED)
shard-router balance                             # per-shard cash, exact centicents
shard-router fees    KXNFLGAME 8600              # exact taker/maker fees at a price
shard-router prefund KX...-TICKER 15.00 --wait   # move shortfall, wait until LANDED
shard-router ledger                             # transfer history + live reconcile
shard-router exposure                           # positions + orders per shard
shard-router sim-world 42                        # DST world sim, replayable
```

`--json` emits machine-readable output on every command. Exit codes are a
stable gate contract:

| exit | meaning | pipeline use |
|---|---|---|
| 0 | VERIFIED | proceed |
| 3 | gated hint | pre-position funds only |
| 4 | unknown | stop |
| 5 | insufficient funds | stop, alert |

So a trading script can `route && prefund --wait && place` with no guessing
in the middle.

## Fee math, validated against production

Fees are exact integer math (coefficients in integer units, explicit
rounding), read live from the series endpoint — never hardcoded:

| series class | taker @ 50c | maker @ 50c | validation |
|---|---|---|---|
| `quadratic` (e.g. Challenger tennis) | 175cc | 0 | published schedule |
| `quadratic_with_maker_fees` (e.g. NFL) | 175cc | 44cc | published schedule |
| 0.5x multiplier lanes (e.g. MLB) | 88cc | 22cc | halved exactly |
| any, @ 86c | 84cc | — | **matched a real fill to the cent** |

The per-contract taker fee is `0.07 * p * (1-p) * multiplier`; maker lanes add
`0.0176 * p * (1-p) * multiplier`. Integer centicents all the way down.

## TigerStyle invariants

- **Money is integer centicents** (`1 dollar = 10_000`), parsed from
  fixed-point strings with manual digit arithmetic — the `$15 = 1500`
  centicent bug is encoded as a regression test.
- **Checked arithmetic everywhere**; overflow is a typed error, never a
  panic. `panic = "abort"` in release.
- **Closed-set shard ids**: `Shard::from_index` rejects anything outside
  the verified 0..=3 set at parse time.
- **Pure core, thin adapters**: routing, funding plans, fee math, and the
  state machines are pure functions, fully tested without I/O.
- **Refusal semantics**: pre-funding guarantees availability or returns a
  typed `Insufficient` — never a silent partial transfer.
- **Zero warnings** under `cargo build`, `clippy`, and `test` (CI-enforced).

## DST (deterministic simulation testing)

The VOPR pattern, applied to exchange operations. Two pure state machines,
both replayable by seed — same seed, same terminal state, forever:

- **`sim <seed>`** — the prefund-wait machine. The live adapter feeds real
  balance observations through the *same* transition the simulator
  exercises, so a failing simulation is provably the same bug class as a
  failing live path. Named regressions: never-fake-lands, stuck-bounded.

- **`sim-world <seed>`** — a multi-agent shard universe: PRNG-shaped agents
  issue transfers and orders with racing landing/fill ticks. Invariants
  checked after *every* transition:
  - **INV-1** cash conservation (balances + in-flight + filled == initial)
  - **INV-2** fills draw only *observed* cash — the cash-clamp bug class,
    replayable via the named scenario `sim-world clamp`
  - **INV-3** every world terminates within a bounded tick budget

A failing seed is a coordinate, not a story: `shard-router sim-world <seed>`
reproduces any reported bug forever.

The **ledger** is append-only JSONL, idempotent on `transfer_id`, tolerant
of torn final writes. `reconcile` diffs ledger-expected vs live per shard
and labels drift honestly: settlements, fills, and fees the ledger does
not know about show as labeled drift — a signal to investigate, never an
assumption that money vanished.

## The shard map (empirical, verified 2026-09-29)

```
0  US macro / weather (KXHIGH*, KXLOW*) / major US sports / CPI / politics
1  MVE crosscategory microcap events
2  crypto + commodity dailies (BTC, ETH, XRP, copper)
3  tennis + international sports (ATP, WTA, EPL, Bundesliga, WNBA totals)
```

Established by transfer-endpoint enum probing (destinations beyond 3 are
rejected as unknown shards) and routing the full series listing. Treat it
as a cache: `route` verifies against the live API.

## Roadmap

- `cargo install shard-router` distribution (crates.io)
- `exposure` notional sizing (mark-to-market per shard, not just counts)

## License

MIT — see [LICENSE](LICENSE). Tiger Style is an engineering philosophy from
[TigerBeetle](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md);
this project is independent of TigerBeetle.
