# Security Policy

## Supported versions

Only the latest tagged release receives security fixes.

## Reporting a vulnerability

Email the maintainer via the GitHub "Report a vulnerability" flow on the
Security tab, or open a private security advisory. Please do not open a
public issue describing credentials handling, signing, or anything that
could expose API keys.

Include: the commit, a minimal reproduction, and — for anything in the
DST harness — the failing seed (`shard-router sim-world <seed>` coordinates
are reproducible forever).

## Scope notes specific to this project

- The tool holds **no credentials by default**. API keys are read from
  environment variables only; nothing is written to disk, logs, or the
  ledger. Transfers are recorded as `(id, shards, amount)` — amounts are
  money, but no secrets ever enter the append-only log.
- The ledger is append-only and world-readable only to the invoking user;
  point `SHARD_ROUTER_LEDGER` at a directory with appropriate permissions
  if your threat model includes a shared `$HOME`.
- Money is integer centicents end-to-end; the classic failure modes
  (unit truncation, float drift, partial transfers) are typed errors and
  tests, not mitigations.

## Disclosure

Reporters get credit in the release notes unless they prefer anonymity.
Fixes ship as patch releases with a `CHANGELOG.md` entry once a fix is
available on the default branch.
