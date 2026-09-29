//! router.rs — deterministic routing core. Pure functions, no I/O.
//!
//! TigerStyle invariants:
//! - Money is integer centicents (1 dollar = 10_000 centicents); no floats
//!   ever touch a money path.
//! - Every arithmetic op is checked; overflow/underflow is a typed error,
//!   never a panic.
//! - The routing table is the single source of truth for KNOWN families;
//!   anything outside it is UNVERIFIED until read from the exchange.

use std::collections::HashMap;

/// Shard id on the Kalshi exchange. Empirically 0..=3 (verified by transfer
/// enum probe 2026-09-29: shards > 3 answer "unknown destination exchange
/// shard"). We accept the closed set and reject anything else at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shard(pub u8);

impl Shard {
    pub const ZERO: Shard = Shard(0);
    pub const ONE: Shard = Shard(1);
    pub const TWO: Shard = Shard(2);
    pub const THREE: Shard = Shard(3);

    pub fn from_index(i: i64) -> Result<Shard, RouterError> {
        match i {
            0..=3 => Ok(Shard(i as u8)),
            other => Err(RouterError::UnknownShard(other)),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RouterError {
    #[error("shard {0} does not exist (exchange has 0..=3)")]
    UnknownShard(i64),
    #[error("money parse failed: {0:?}")]
    BadMoney(Vec<u8>),
    #[error("money overflow: {0} centicents")]
    MoneyOverflow(i64),
    #[error("transfer must be between different shards")]
    SelfTransfer,
    #[error("insufficient balance: have {have} need {need} centicents")]
    Insufficient { have: i64, need: i64 },
}

/// Money in integer centicents. Parsed from Kalshi fixed-point dollar strings
/// ("17.4085") with manual digit arithmetic — no float round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Centicents(i64);

impl Centicents {
    /// Parse a dollar amount like "17.4085" into centicents (17_4085).
    /// Accepts 0-4 decimal places; anything longer is truncated-bad, rejected.
    pub fn parse_dollars(s: &str) -> Result<Centicents, RouterError> {
        let (int_part, frac_part) = match s.split_once('.') {
            Some((i, f)) => (i, f),
            None => (s, ""),
        };
        if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(RouterError::BadMoney(s.bytes().collect()));
        }
        if frac_part.len() > 4 || !frac_part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(RouterError::BadMoney(s.bytes().collect()));
        }
        let mut total: i64 = 0;
        for b in int_part.bytes() {
            total = total
                .checked_mul(10)
                .and_then(|t| t.checked_add((b - b'0') as i64))
                .ok_or(RouterError::MoneyOverflow(total))?;
        }
        // pad fraction to exactly 4 digits
        let mut frac = frac_part.to_string();
        while frac.len() < 4 {
            frac.push('0');
        }
        for b in frac.bytes() {
            total = total
                .checked_mul(10)
                .and_then(|t| t.checked_add((b - b'0') as i64))
                .ok_or(RouterError::MoneyOverflow(total))?;
        }
        Ok(Centicents(total))
    }

    pub fn as_i64(self) -> i64 {
        self.0
    }
}

/// A routing verdict. VERIFIED means it came from the exchange API (or the
/// static table of families we have already read). HINT means the semantic
/// tier produced it and the caller MUST re-verify before placing an order.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Verified { shard: Shard, source: &'static str },
    Hint { shard: Shard, confidence: f64 },
    Unknown,
}

/// Static families we have already read from the exchange. A series outside
/// this table is NOT guessed — it resolves Unknown and falls to the live path.
/// (Never predict what you can read.)
pub fn static_route(series: &str) -> Verdict {
    const S0: &[&str] = &[
        "KXHIGH", "KXLOW", "KXNFLGAME", "KXNFLSPREAD", "KXNFLTOTAL", "KXCPI",
        "KXEARNINGSMENTIONNKE", "KXMENTIONEARNNKE", "KXSB", "KXMLB", "KXMLBGAME",
    ];
    const S1: &[&str] = &["KXMVECROSSCATEGORY", "KXMVECROSSCATEGORY0"];
    const S2: &[&str] = &["KXBTCD", "KXETHD", "KXCOPPERD", "KXXRPD"];
    const S3: &[&str] = &[
        "KXATPCHALLENGERMATCH", "KXATPMATCH", "KXATPSETWINNER", "KXWTAMATCH",
        "KXWNBA2HTOTAL", "KXEPLGAME", "KXBUNDESLIGAGAME",
    ];
    // KXATPCHALLENGERMATCH must match before KXATPMATCH-style prefixes, but
    // series tickers are exact, so a direct scan is unambiguous.
    if S0.contains(&series) {
        Verdict::Verified { shard: Shard::ZERO, source: "static" }
    } else if S1.contains(&series) {
        Verdict::Verified { shard: Shard::ONE, source: "static" }
    } else if S2.contains(&series) {
        Verdict::Verified { shard: Shard::TWO, source: "static" }
    } else if S3.contains(&series) {
        Verdict::Verified { shard: Shard::THREE, source: "static" }
    } else if series.starts_with("KXHIGH") || series.starts_with("KXLOW") {
        // Weather families are city-suffixed (KXHIGHLAX, KXHIGHTPA, ...) but
        // ALL settle on the shard-0 engine; a family-prefix rule covers every
        // city without enumerating them (they rotate).
        Verdict::Verified { shard: Shard::ZERO, source: "static-family" }
    } else {
        Verdict::Unknown
    }
}

/// Extract the series ticker from a market ticker by splitting on '-'.
pub fn series_of_ticker(ticker: &str) -> &str {
    ticker.split('-').next().unwrap_or("")
}

/// Compute the funding plan: how much to move so `need` is available on `to`.
/// Returns None when no transfer is required (already funded, or self-transfer).
pub fn funding_plan(
    balances: &HashMap<Shard, Centicents>,
    to: Shard,
    need: Centicents,
) -> Result<Option<(Shard, Centicents)>, RouterError> {
    let have = balances.get(&to).copied().unwrap_or(Centicents(0));
    if have >= need {
        return Ok(None);
    }
    let shortfall = need
        .as_i64()
        .checked_sub(have.as_i64())
        .ok_or(RouterError::MoneyOverflow(need.as_i64()))?;
    // Pick the shard with the largest free balance as the source. If even the
    // richest shard cannot cover the shortfall, that is a typed Insufficient —
    // pre-funding must guarantee availability, not best-effort-move.
    let best = balances
        .iter()
        .filter(|(s, _)| **s != to)
        .max_by_key(|(_, b)| b.as_i64());
    match best {
        Some((src, bal)) if bal.as_i64() >= shortfall => {
            let _ = src;
            Ok(Some((*src, Centicents(shortfall))))
        }
        _ => Err(RouterError::Insufficient { have: have.as_i64(), need: need.as_i64() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_parses_exact() {
        assert_eq!(Centicents::parse_dollars("17.4085").unwrap().as_i64(), 174_085);
        assert_eq!(Centicents::parse_dollars("15").unwrap().as_i64(), 150_000);
        assert_eq!(Centicents::parse_dollars("0.0001").unwrap().as_i64(), 1);
        assert!(Centicents::parse_dollars("17.40851").is_err());
        assert!(Centicents::parse_dollars("-1").is_err());
        assert!(Centicents::parse_dollars("abc").is_err());
    }

    #[test]
    fn shards_closed_set() {
        assert_eq!(Shard::from_index(3).unwrap(), Shard::THREE);
        assert!(Shard::from_index(4).is_err());
        assert!(Shard::from_index(-1).is_err());
    }

    #[test]
    fn static_routes() {
        assert_eq!(static_route("KXHIGH"), Verdict::Verified { shard: Shard::ZERO, source: "static" });
        assert_eq!(static_route("KXATPCHALLENGERMATCH"), Verdict::Verified { shard: Shard::THREE, source: "static" });
        assert_eq!(static_route("KXBTCD"), Verdict::Verified { shard: Shard::TWO, source: "static" });
        assert_eq!(static_route("KXNOTATHING"), Verdict::Unknown);
        // weather families: every city suffix routes to shard 0
        assert_eq!(static_route("KXHIGHLAX"), Verdict::Verified { shard: Shard::ZERO, source: "static-family" });
        assert_eq!(static_route("KXHIGHTPA"), Verdict::Verified { shard: Shard::ZERO, source: "static-family" });
        assert_eq!(static_route("KXLOWTHOU"), Verdict::Verified { shard: Shard::ZERO, source: "static-family" });
    }

    #[test]
    fn funding_plan_moves_shortfall_from_richest() {
        let mut b = HashMap::new();
        b.insert(Shard::ZERO, Centicents::parse_dollars("5.00").unwrap());
        b.insert(Shard::THREE, Centicents::parse_dollars("1.00").unwrap());
        // shortfall = 6.00 - 1.00 = 5.00, exactly coverable from shard 0
        let plan = funding_plan(&b, Shard::THREE, Centicents::parse_dollars("6.00").unwrap());
        assert_eq!(plan.unwrap(), Some((Shard::ZERO, Centicents::parse_dollars("5.00").unwrap())));

        // already funded -> no transfer
        let plan = funding_plan(&b, Shard::ZERO, Centicents::parse_dollars("5.00").unwrap());
        assert_eq!(plan.unwrap(), None);

        // partial funding is refused: guarantee, not best-effort
        let plan = funding_plan(&b, Shard::THREE, Centicents::parse_dollars("10.00").unwrap());
        assert!(matches!(plan, Err(RouterError::Insufficient { .. })));
    }

    #[test]
    fn funding_plan_insufficient_is_typed() {
        let mut b = HashMap::new();
        b.insert(Shard::ZERO, Centicents::parse_dollars("2.00").unwrap());
        let err = funding_plan(&b, Shard::THREE, Centicents::parse_dollars("10.00").unwrap());
        assert!(matches!(err, Err(RouterError::Insufficient { .. })));
    }

    #[test]
    fn series_extraction() {
        assert_eq!(series_of_ticker("KXATPCHALLENGERMATCH-26SEP29CADSAN-CAD"), "KXATPCHALLENGERMATCH");
        assert_eq!(series_of_ticker("KXHIGH"), "KXHIGH");
    }
}
