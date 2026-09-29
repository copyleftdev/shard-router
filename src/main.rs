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
//!
//! Machine interface: `--json` emits a single JSON object; exit codes are a
//! stable gate contract for scripts and crons:
//!   0 VERIFIED | 3 gated hint | 4 unknown | 5 insufficient funds

mod dst;
mod jev;
mod kalshi;
mod router;

use router::{Centicents, Shard, Verdict};
use std::collections::HashMap;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let json = args.iter().any(|a| a == "--json");
    let args: Vec<String> = args.into_iter().filter(|a| a != "--json").collect();
    match run(&args, json) {
        Ok(code) => code,
        Err(msg) => {
            if json {
                println!("{{\"error\": {}}}", serde_json::json!(msg));
            } else {
                eprintln!("shard-router: {msg}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String], json: bool) -> Result<ExitCode, String> {
    match args.get(1).map(String::as_str) {
        Some("route") => {
            let target = args.get(2).ok_or("usage: shard-router route <MARKET-OR-SERIES-TICKER>")?;
            route(target, json)
        }
        Some("classify") => {
            let desc = args.get(2).ok_or("usage: shard-router classify <description>")?;
            classify(desc, json)
        }
        Some("balance") => balance(json),
        Some("fees") => {
            let target = args.get(2).ok_or("usage: shard-router fees <SERIES-OR-MARKET-TICKER> [price_centicents]")?;
            let price = args.get(3).map(|s| s.parse::<i64>()).transpose().map_err(|e| e.to_string())?;
            fees(target, price, json)
        }
        Some("prefund") => {
            let ticker = args.get(2).ok_or("usage: shard-router prefund <MARKET-TICKER> <DOLLARS> [--wait]")?;
            let dollars = args.get(3).ok_or("prefund needs a dollar amount")?;
            let wait = args.iter().any(|a| a == "--wait");
            prefund(ticker, dollars, wait, json)
        }
        Some("shards") => {
            if json {
                println!("{{\"shards\": [0, 1, 2, 3]}}");
            } else {
                println!("exchange shards: 0 (US macro/weather/sports) 1 (MVE microcap) 2 (crypto/commodity) 3 (tennis/international)");
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("sim") => {
            // DST harness: run the prefund-wait simulator at a seed. Same
            // seed => same world => same terminal state (the replay promise).
            let seed = args.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(1);
            let st = dst::sim_once(seed);
            println!("seed {seed} -> {st:?}");
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            eprintln!("usage: shard-router [--json] <route|classify|balance|fees|prefund|shards> [...]");
            eprintln!("  route <ticker>             resolve shard: static table -> live API (exit 0/3/4)");
            eprintln!("  classify <text>            semantic hint via jev (gated; UNVERIFIED, exit 3/4)");
            eprintln!("  balance                    per-shard balances, exact centicents");
            eprintln!("  fees <series> [price_cc]   fee schedule + exact per-contract fees at a price");
            eprintln!("  prefund <t> $N [--wait]    ensure shard holding <t> has $N (exit 5 = insufficient)");
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn route(target: &str, json: bool) -> Result<ExitCode, String> {
    let series = router::series_of_ticker(target);
    let static_v = router::static_route(series);
    let mut final_v = static_v.clone();
    let ks = kalshi::Kalshi::from_env()?;
    let mut live: Option<Verdict> = None;
    if let Ok(ms) = ks.market_shard(target) {
        live = Some(Verdict::Verified { shard: ms.shard, source: "live" });
        final_v = live.clone().unwrap();
    }
    if !json {
        print_verdict(&static_v, "static");
        if let Some(l) = &live {
            print_verdict(l, "live");
        } else if matches!(static_v, Verdict::Unknown) {
            println!("live   -> no market data (series unknown to table and exchange)");
            println!("hint   -> use `shard-router classify <description>` for a semantic guess (UNVERIFIED)");
        }
    }
    emit_json(json, |o| {
        o.insert("verdict".into(), serde_json::to_value(&final_v).unwrap_or_default());
    });
    Ok(match final_v {
        Verdict::Verified { .. } => ExitCode::SUCCESS,
        Verdict::Hint { .. } => ExitCode::from(3),
        Verdict::Unknown => ExitCode::from(4),
    })
}

fn classify(desc: &str, json: bool) -> Result<ExitCode, String> {
    let j = jev::Jev::from_env()?;
    let v = j.classify_market(desc)?;
    if !json {
        print_verdict(&v, "jev");
        if matches!(v, Verdict::Hint { .. }) {
            println!("NOTE: semantic hints are for fund pre-positioning only.");
            println!("      Order placement MUST re-read the shard via `route` (API is the truth).");
        } else {
            println!("confidence below gate ({}); refusing to route", jev::CONFIDENCE_GATE);
        }
    }
    emit_json(json, |o| {
        o.insert("verdict".into(), serde_json::to_value(&v).unwrap_or_default());
    });
    Ok(match v {
        Verdict::Hint { .. } => ExitCode::from(3),
        _ => ExitCode::from(4),
    })
}

fn balance(json: bool) -> Result<ExitCode, String> {
    let ks = kalshi::Kalshi::from_env()?;
    let bals = ks.balances()?;
    let mut rows: Vec<(Shard, Centicents)> = bals.into_iter().collect();
    rows.sort_by_key(|(s, _)| s.0);
    emit_json(json, |o| {
        let map: serde_json::Map<String, serde_json::Value> = rows
            .iter()
            .map(|(s, b)| {
                (format!("shard_{}", s.0), serde_json::json!({ "centicents": b.as_i64() }))
            })
            .collect();
        o.insert("balances".into(), serde_json::Value::Object(map));
    });
    if !json {
        for (shard, bal) in rows {
            let c = bal.as_i64();
            println!("shard {}: ${}.{:04} ({} centicents)", shard.0, c / 10_000, c.rem_euclid(10_000), c);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn fees(target: &str, price: Option<i64>, json: bool) -> Result<ExitCode, String> {
    let series = router::series_of_ticker(target);
    let ks = kalshi::Kalshi::from_env()?;
    let sched = ks.series_fees(series)?;
    let fee_type = if sched.maker_fees { "quadratic_with_maker_fees" } else { "quadratic" };
    let mut o = serde_json::Map::new();
    o.insert("series".into(), serde_json::json!(series));
    o.insert("fee_type".into(), serde_json::json!(fee_type));
    o.insert("multiplier".into(), serde_json::json!(sched.multiplier_pm as f64 / 1000.0));
    if let Some(p) = price {
        let taker = sched.taker_fee_centicents(p).map_err(|e| e.to_string())?;
        let maker = sched.maker_fee_centicents(p).map_err(|e| e.to_string())?;
        o.insert("price_centicents".into(), serde_json::json!(p));
        o.insert("taker_fee_centicents".into(), serde_json::json!(taker));
        o.insert("maker_fee_centicents".into(), serde_json::json!(maker));
        if !json {
            println!("series {series}: {fee_type} x{}", sched.multiplier_pm as f64 / 1000.0);
            println!("  taker fee at {}cc: {}cc (${:.4})", p, taker, taker as f64 / 10_000.0);
            println!("  maker fee at {}cc: {}cc (${:.4})", p, maker, maker as f64 / 10_000.0);
        }
    } else if !json {
        println!("series {series}: {fee_type} x{}", sched.multiplier_pm as f64 / 1000.0);
        println!("  pass a price in centicents for exact per-contract fees: `fees {series} 5800`");
    }
    if json {
        println!("{}", serde_json::Value::Object(o));
    }
    Ok(ExitCode::SUCCESS)
}

fn prefund(ticker: &str, dollars: &str, wait: bool, json: bool) -> Result<ExitCode, String> {
    let ks = kalshi::Kalshi::from_env()?;
    let need = Centicents::parse_dollars(dollars).map_err(|e| e.to_string())?;
    let ms = ks.market_shard(ticker)?;
    let bals: HashMap<Shard, Centicents> = ks.balances()?;
    match router::funding_plan(&bals, ms.shard, need) {
        Ok(None) => {
            if !json {
                println!("shard {} already holds >= ${}; no transfer needed", ms.shard.0, dollars);
            }
            emit_json(json, |o| {
                o.insert("action".into(), serde_json::json!("none"));
                o.insert("shard".into(), serde_json::json!(ms.shard.0));
            });
            Ok(ExitCode::SUCCESS)
        }
        Ok(Some((src, amt))) => {
            let c = amt.as_i64();
            let id = ks.transfer(src, ms.shard, c)?;
            let landed = if wait {
                ks.wait_for_landed(ms.shard, need)?
            } else {
                crate::dst::WaitState::Waiting { polls: 0, need }
            };
            let ok = matches!(landed, crate::dst::WaitState::Landed { .. });
            emit_json(json, |o| {
                o.insert("action".into(), serde_json::json!("transfer"));
                o.insert("transfer_id".into(), serde_json::json!(id));
                o.insert("from_shard".into(), serde_json::json!(src.0));
                o.insert("to_shard".into(), serde_json::json!(ms.shard.0));
                o.insert("centicents".into(), serde_json::json!(c));
                o.insert("landed".into(), serde_json::json!(ok));
            });
            if !json {
                println!("moving ${}.{:04} ({} cc) shard {} -> {} for {ticker}", c / 10_000, c.rem_euclid(10_000), c, src.0, ms.shard.0);
                println!("transfer {id} accepted (async)");
                if wait {
                    match landed {
                        crate::dst::WaitState::Landed { polls } => {
                            println!("LANDED: cash observed on shard {} after {polls} polls", ms.shard.0);
                        }
                        crate::dst::WaitState::Stuck { .. } => {
                            eprintln!("STUCK: transfer not observed within budget; do NOT order yet");
                        }
                        crate::dst::WaitState::Waiting { .. } => {
                            println!("(async: re-poll balance before ordering, or pass --wait)");
                        }
                    }
                } else {
                    println!("(async: re-poll balance before ordering, or pass --wait)");
                }
            }
            Ok(if ok || !wait { ExitCode::SUCCESS } else { ExitCode::FAILURE })
        }
        Err(e) => {
            emit_json(json, |o| {
                o.insert("action".into(), serde_json::json!("insufficient"));
                o.insert("error".into(), serde_json::json!(e.to_string()));
            });
            if !json {
                eprintln!("{e}");
            }
            Ok(ExitCode::from(5))
        }
    }
}

fn print_verdict(v: &Verdict, tier: &str) {
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
}

fn emit_json(json: bool, fill: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>)) {
    if !json {
        return;
    }
    let mut o = serde_json::Map::new();
    fill(&mut o);
    println!("{}", serde_json::Value::Object(o));
}
