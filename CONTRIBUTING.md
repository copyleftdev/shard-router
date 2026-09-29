# Contributing

Thanks for considering a contribution. This project follows a small set of
non-negotiable engineering rules (TigerBeetle's [Tiger Style](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md)
is the reference). PRs that violate them will be asked to change.

## Ground rules

1. **No floats on money paths.** Money is integer centicents
   (`1 dollar = 10_000`). Parsing uses manual digit arithmetic, and fees use
   integer coefficient math with explicit rounding. If a change introduces
   an `f64` that ever touches money, it is wrong.
2. **No panics in production paths.** No `unwrap`/`expect`/indexing that can
   panic outside `assert!` invariants and tests. Errors are typed
   (`thiserror`).
3. **Never predict what you can read.** Shard resolution is VERIFIED only
   via the exchange API (or the static table read from it). Model output is
   a gated HINT and can never be promoted to VERIFIED by any code path.
4. **All I/O at the edges.** Core logic (routing, funding plans, fee math,
   the state machines) is pure and unit-tested without network or clock.
   Live adapters are thin.
5. **Bounded everything.** Poll loops, ledgers, retries, and simulations
   carry explicit budgets; exceeding one is a typed error, not a hang.
6. **Zero warnings.** `cargo build`, `cargo clippy`, and `cargo test` must
   be warning-free in CI.

## Deterministic simulation testing

New order/transfer/funding semantics belong in `src/world.rs` (or a new pure
state machine module) with invariants checked after every transition, plus
named regression tests for each discovered failure mode. A failing seed is
a coordinate: `shard-router sim-world <seed>` must reproduce any bug report.

## Workflow

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Please add a `CHANGELOG.md` entry under an `Unreleased` heading for
user-visible changes. Security-sensitive issues go to the process in
`SECURITY.md` — do not open a public issue for credentials or signing bugs.

## Commit style

Conventional commits (`feat:`, `fix:`, `docs:`, `test:`, `refactor:`). Keep
commits small enough to review in isolation.
