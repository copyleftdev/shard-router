//! ledger.rs — append-only transfer ledger + balance reconciliation (roadmap
//! item 4). Every transfer the router executes is recorded with its
//! exchange-issued id (idempotent on re-run); `reconcile` diffs the
//! ledger-expected balances against live observations and labels the drift
//! per shard (unlogged activity: order fills, settlements, fees).
//!
//! Money is integer centicents; entries are newline-delimited JSON.

use crate::router::{Centicents, Shard};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;

pub fn ledger_path() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("SHARD_ROUTER_LEDGER") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    Ok(PathBuf::from(home).join(".local/share/shard-router/ledger.jsonl"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferRecord {
    pub transfer_id: String,
    pub from_shard: u8,
    pub to_shard: u8,
    pub centicents: i64,
    /// unix seconds
    pub ts: u64,
    /// terminal state once known: "landed" | "unknown" at record time
    pub observed: String,
}

/// Append a transfer record. Idempotent on transfer_id: a re-run with the
/// same id is a no-op (pair-asserted by reading back after write).
pub fn record_transfer(rec: &TransferRecord) -> Result<(), String> {
    let path = ledger_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let existing = load_transfers()?;
    let mut ids: HashSet<String> = HashSet::new();
    for r in &existing {
        ids.insert(r.transfer_id.clone());
    }
    if ids.contains(&rec.transfer_id) {
        return Ok(()); // idempotent
    }
    let line = serde_json::to_string(rec).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{line}").map_err(|e| e.to_string())?;
    // pair assertion: the record we just wrote is readable back
    let after = load_transfers()?;
    assert!(
        after.iter().any(|r| r.transfer_id == rec.transfer_id),
        "ledger write not read back — idempotency contract broken"
    );
    Ok(())
}

/// Load all transfer records, tolerating a torn final line (a partially
/// written line from a crash is skipped, never parsed).
pub fn load_transfers() -> Result<Vec<TransferRecord>, String> {
    let path = ledger_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let txt = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for line in txt.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(r) = serde_json::from_str::<TransferRecord>(line) {
            out.push(r);
        }
        // unparseable trailing line: skipped (torn write), bounded loss
    }
    Ok(out)
}

/// Expected per-shard cash deltas from the ledger alone.
pub fn ledger_deltas(records: &[TransferRecord]) -> HashMap<Shard, i64> {
    let mut out: HashMap<Shard, i64> = HashMap::new();
    for r in records {
        let from = Shard(r.from_shard);
        let to = Shard(r.to_shard);
        *out.entry(from).or_insert(0) -= r.centicents;
        *out.entry(to).or_insert(0) += r.centicents;
    }
    out
}

#[derive(Debug, PartialEq)]
pub struct Drift {
    pub shard: Shard,
    /// live observed balance
    pub live: Centicents,
    /// ledger-expected delta applied to the first-ever observed snapshot
    pub expected: Centicents,
    /// live - expected, in centicents: unlogged activity (fills, settlements)
    pub drift_cc: i64,
}

/// Reconcile: apply ledger deltas to the opening snapshot and diff live.
/// Positive drift = unlogged inflow (settlements/wins); negative = unlogged
/// outflow (fills/fees). Drift is a signal to investigate, never money lost
/// — the ledger states honestly what it does not know.
pub fn reconcile(
    opening: &HashMap<Shard, Centicents>,
    live: &HashMap<Shard, Centicents>,
    records: &[TransferRecord],
) -> Vec<Drift> {
    let deltas = ledger_deltas(records);
    let mut shards: HashSet<Shard> = HashSet::new();
    shards.extend(opening.keys().copied());
    shards.extend(live.keys().copied());
    let mut out = Vec::new();
    for s in shards {
        let open = opening.get(&s).copied().unwrap_or(Centicents(0));
        let delta = deltas.get(&s).copied().unwrap_or(0);
        let expected = Centicents(open.as_i64() + delta);
        let lv = live.get(&s).copied().unwrap_or(Centicents(0));
        out.push(Drift {
            shard: s,
            live: lv,
            expected,
            drift_cc: lv.as_i64() - expected.as_i64(),
        });
    }
    out.sort_by_key(|d| d.shard.0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, from: u8, to: u8, cc: i64) -> TransferRecord {
        TransferRecord {
            transfer_id: id.into(),
            from_shard: from,
            to_shard: to,
            centicents: cc,
            ts: 0,
            observed: "unknown".into(),
        }
    }

    #[test]
    fn ledger_roundtrip_and_idempotency() {
        // use a temp ledger via env var
        let dir = std::env::temp_dir().join(format!("sr-ledger-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SHARD_ROUTER_LEDGER", dir.join("l.jsonl"));
        let r = rec("t1", 0, 3, 150_000);
        record_transfer(&r).unwrap();
        record_transfer(&r).unwrap(); // idempotent: no duplicate
        let loaded = load_transfers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], r);
        // torn trailing line tolerated
        std::fs::write(
            dir.join("l.jsonl"),
            format!("{}\n{{\"torn", serde_json::to_string(&r).unwrap()),
        )
        .unwrap();
        let loaded = load_transfers().unwrap();
        assert_eq!(loaded.len(), 1);
        std::env::remove_var("SHARD_ROUTER_LEDGER");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_labels_drift_per_shard() {
        // opening: $5 on 0, $1 on 3. Transfer $4 0->3. Live: 0 has $1, 3 has
        // $5 => zero drift. Then simulate a $10 settlement win on 3 (+drift).
        let mut opening = HashMap::new();
        opening.insert(Shard::ZERO, Centicents(50_000));
        opening.insert(Shard::THREE, Centicents(10_000));
        let records = vec![rec("t1", 0, 3, 40_000)];
        let mut live = HashMap::new();
        live.insert(Shard::ZERO, Centicents(10_000));
        live.insert(Shard::THREE, Centicents(50_000));
        let d = reconcile(&opening, &live, &records);
        assert!(d.iter().all(|x| x.drift_cc == 0), "drifts: {d:?}");
        // settlement lands: +100_000cc on shard 3, unlogged
        live.insert(Shard::THREE, Centicents(150_000));
        let d = reconcile(&opening, &live, &records);
        let s3 = d.iter().find(|x| x.shard == Shard::THREE).unwrap();
        assert_eq!(s3.drift_cc, 100_000);
    }

    #[test]
    fn deltas_never_create_cash() {
        let records = vec![
            rec("a", 0, 3, 100),
            rec("b", 3, 2, 100),
            rec("c", 2, 1, 100),
        ];
        let d = ledger_deltas(&records);
        let sum: i64 = d.values().sum();
        assert_eq!(sum, 0, "ledger deltas must be zero-sum");
    }
}
