# shard-router

Deterministic-first semantic shard router for the Kalshi exchange.

Kalshi runs 4 parallel matching engines ("shards", `exchange_index` 0-3,
verified by transfer enum probe). Cash is held **per shard** and orders
draw only from their market's shard — routing funds wrong means bounced
orders (`insufficient_balance`) even when the account is flush.

## The tier model — never predict what you can read

| tier | mechanism | cost | authority |
|------|-----------|------|-----------|
| 1 | static table of verified series families | zero | VERIFIED |
| 2 | live exchange read (`GET /markets/{t}` → `exchange_index`) | 1 API call | VERIFIED (only truth) |
| 3 | Jev (TypeSafe System One) semantic classification of free text | 1 model call | **HINT only** — confidence-gated at 0.80, used solely to pre-position funds |

A tier-3 hint NEVER substitutes for a tier-2 read at order placement time.
Below-gate hints surface as UNKNOWN rather than routing.

## TigerStyle invariants

- **Money is integer centicents** (`1 dollar = 10_000`), parsed from
  fixed-point strings with manual digit arithmetic. No float ever touches a
  money path — the `$15 = 1500` centicent bug is encoded as a test.
- **Checked arithmetic everywhere**; overflow/underflow is a typed
  `RouterError`, never a panic. `panic = "abort"` in release.
- **Closed-set shard ids**: `Shard::from_index` rejects anything outside
  0..=3 at parse time.
- **Deterministic core**: routing and funding plans are pure functions,
  fully unit-tested without I/O; live adapters are thin.
- **Refusal semantics**: pre-funding guarantees availability or returns a
  typed `Insufficient` — never a silent partial transfer.
- Zero warnings, no `unwrap` in production paths.

## Usage

```
shard-router shards                            # the shard map
shard-router route  KXATPCHALLENGERMATCH-...   # static -> live resolution
shard-router classify "BTC daily range market" # jev semantic hint (UNVERIFIED)
shard-router balance                           # per-shard cash, exact centicents
shard-router fees    KXNFLGAME 8600            # exact taker/maker fees at a price
shard-router prefund KX...-TICKER 15.00 --wait # move shortfall, wait until LANDED
shard-router sim     42                        # DST harness: replay a seed
```

`--json` emits machine-readable output on every command. Exit codes are a
stable gate contract: `0` VERIFIED, `3` gated hint, `4` unknown,
`5` insufficient funds — so scripts can `route && prefund --wait && place`.

Fee math is exact integer centicents, validated against a live fill
(KXHIGHTOKC NO @ 86c → 84cc taker, to the cent) and the published fee
schedule (50c max: taker 175cc, maker 44cc; 0.5x multiplier lanes halve it).

## DST (VOPR pattern)

The prefund-wait flow is a pure state machine `transition(state, event)`;
all entropy (balance observations, transfer landing) is injected. The live
adapter feeds real observations through the same transition the simulator
exercises, and `sim <seed>` replays a world: same seed => same terminal
state. Named regressions pin the two failure modes (never fake-lands,
stuck transfers bounded) and the live fee anchors.

Credentials: `API_KEY_ID` env (`/home/ops/.creds/kalshi.env`), private key at
`/home/ops/.creds/kalshi.pk` (RSA-PSS signing, same scheme as the kalshi-chaos
feed). Jev key read from `/home/ops/.creds/typsafe_labs.env`.

## Shard map (empirical, 2026-09-29)

```
0  US macro / weather (KXHIGH*, KXLOW*) / NFL / MLB / CPI / politics
1  MVE crosscategory microcap events
2  crypto + commodity dailies (KXBTCD, KXETHD, KXXRPD, KXCOPPERD)
3  tennis + international sports (ATP/WTA, EPL, Bundesliga, WNBA totals)
```
