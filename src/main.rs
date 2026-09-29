//! shard-router — deterministic-first semantic shard router for Kalshi.
//!
//! Tiers:
//!   1. static table of verified families   (zero model calls)
//!   2. live exchange read                  (authoritative, always)
//!   3. jev semantic hint                   (unknown series / free text,
//!      confidence-gated; a hint NEVER substitutes for the live read at
//!      order-placement time)
//!
//! Money is integer centicents everywhere. No floats on money paths. No
//! panics in production code: every fallible operation is a typed Result.

mod jev;
mod kalshi;
mod router;

use router::{Centicents, Shard, Verdict};
use std::collections::HashMap;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("shard-router: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args.get(1).map(String::as_str) {
        Some("route") => {
            let target = args.get(2).ok_or("usage: shard-router route <MARKET-OR-SERIES-TICKER>")?;
            route(target)
        }
        Some("classify") => {
            let desc = args.get(2).ok_or("usage: shard-router classify <description>")?;
            classify(desc)
        }
        Some("balance") => balance(),
        Some("prefund") => {
            let ticker = args.get(2).ok_or("usage: shard-router prefund <MARKET-TICKER> <DOLLARS>")?;
            let dollars = args.get(3).ok_or("prefund needs a dollar amount")?;
            prefund(ticker, dollars)
        }
        Some("shards") => {
            println!("exchange shards: 0 (US macro/weather/sports) 1 (MVE microcap) 2 (crypto/commodity) 3 (tennis/international)");
            Ok(())
        }
        _ => {
            eprintln!("usage: shard-router <route|classify|balance|prefund|shards> [...]");
            eprintln!("  route <ticker>     resolve shard: static table -> live API -> (nothing is guessed)");
            eprintln!("  classify <text>    semantic hint via jev (confidence-gated, UNVERIFIED)");
            eprintln!("  balance            per-shard balances in exact centicents");
            eprintln!("  prefund <t> $N     ensure shard holding <t> has $N (transfers shortfall, centicents-exact)");
            Ok(())
        }
    }
}

fn route(target: &str) -> Result<(), String> {
    let series = router::series_of_ticker(target);
    print_verdict(&router::static_route(series), "static")?;
    // Tier 2: live read — always attempt; it is the only Verified truth for
    // anything outside the static table.
    let ks = kalshi::Kalshi::from_env()?;
    match ks.market_shard(target) {
        Ok(ms) => {
            println!(
                "live   -> VERIFIED shard {} (market status: {})",
                ms.shard.0, ms.status
            );
            Ok(())
        }
        Err(e) => {
            // A bare series (not a market ticker) 404s on the market path; the
            // static table already answered or the series is unknown.
            if matches!(router::static_route(series), Verdict::Unknown) {
                println!("live   -> no market data (series unknown to table and exchange): {e}");
                println!("hint   -> use `shard-router classify <description>` for a semantic guess (UNVERIFIED)");
            }
            Ok(())
        }
    }
}

fn classify(desc: &str) -> Result<(), String> {
    let j = jev::Jev::from_env()?;
    let v = j.classify_market(desc)?;
    print_verdict(&v, "jev")?;
    match v {
        Verdict::Hint { shard, .. } => {
            println!("NOTE: semantic hints are for fund pre-positioning only.");
            println!("      Order placement MUST re-read the shard via `route` (API is the truth).");
            let _ = shard;
            Ok(())
        }
        _ => {
            println!("confidence below gate ({}); refusing to route", jev::CONFIDENCE_GATE);
            Ok(())
        }
    }
}

fn balance() -> Result<(), String> {
    let ks = kalshi::Kalshi::from_env()?;
    let bals = ks.balances()?;
    let mut rows: Vec<(Shard, Centicents)> = bals.into_iter().collect();
    rows.sort_by_key(|(s, _)| s.0);
    for (shard, bal) in rows {
        let c = bal.as_i64();
        println!("shard {}: ${}.{:04} ({} centicents)", shard.0, c / 10_000, c.rem_euclid(10_000), c);
    }
    Ok(())
}

fn prefund(ticker: &str, dollars: &str) -> Result<(), String> {
    let ks = kalshi::Kalshi::from_env()?;
    let need = Centicents::parse_dollars(dollars).map_err(|e| e.to_string())?;
    let ms = ks.market_shard(ticker)?;
    let bals: HashMap<Shard, Centicents> = ks.balances()?;
    match router::funding_plan(&bals, ms.shard, need) {
        Ok(None) => {
            println!("shard {} already holds >= ${}; no transfer needed", ms.shard.0, dollars);
            Ok(())
        }
        Ok(Some((src, amt))) => {
            let c = amt.as_i64();
            println!(
                "moving ${}.{:04} ({} centicents) shard {} -> {} for {ticker}",
                src.0, ms.shard.0, c / 10_000, c.rem_euclid(10_000), c
            );
            let id = ks.transfer(src, ms.shard, c)?;
            println!("transfer accepted: {id} (async: re-poll balance before ordering)");
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn print_verdict(v: &Verdict, tier: &str) -> Result<(), String> {
    match v {
        Verdict::Verified { shard, source } => {
            println!("{tier:6} -> VERIFIED shard {} ({source})", shard.0);
        }
        Verdict::Hint { shard, confidence } => {
            println!("{tier:6} -> HINT shard {} (confidence {confidence:.2}, UNVERIFIED)", shard.0);
        }
        Verdict::Unknown => {
            println!("{tier:6} -> UNKNOWN (no guess without the API or a gated hint)");
        }
    }
    Ok(())
}
