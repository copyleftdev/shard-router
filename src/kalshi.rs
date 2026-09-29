//! kalshi.rs — thin live adapter: signed REST reads (market, balance) and the
//! intra-shard transfer. Money never becomes a float on this path.

use crate::router::{Centicents, RouterError, Shard};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pss::SigningKey;
use rsa::sha2::Sha256;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Kalshi {
    key_id: String,
    signing_key: SigningKey<Sha256>,
    agent: ureq::Agent,
}

#[derive(Debug)]
pub struct MarketShard {
    pub shard: Shard,
    /// Market status at read time (active/finalized/...). Kept for the
    /// caller's logs; the router itself only needs the shard.
    #[allow(dead_code)]
    pub status: String,
}

impl Kalshi {
    /// Credentials: reads API_KEY_ID and KALSHI_PRIVATE_KEY (PEM) from env.
    pub fn from_env() -> Result<Kalshi, String> {
        let key_id = std::env::var("API_KEY_ID").map_err(|e| e.to_string())?;
        let pem_path = std::env::var("KALSHI_PRIVATE_KEY_PATH")
            .unwrap_or_else(|_| "/home/ops/.creds/kalshi.pk".into());
        let pem = std::fs::read_to_string(&pem_path).map_err(|e| e.to_string())?;
        let key = rsa::RsaPrivateKey::from_pkcs1_pem(&pem).map_err(|e| e.to_string())?;
        Ok(Kalshi {
            key_id,
            signing_key: SigningKey::<Sha256>::new(key),
            agent: ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(20)).build(),
        })
    }

    fn sign(&self, ts: &str, method: &str, path: &str) -> String {
        let msg = format!("{ts}{method}{path}");
        let sig = self
            .signing_key
            .sign_with_rng(&mut rsa::rand_core::OsRng, msg.as_bytes());
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, sig.to_bytes())
    }

    fn get(&self, path: &str) -> Result<serde_json::Value, String> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis()
            .to_string();
        // the signature covers ONLY the path — the query string is excluded
        // (verified against kalshi_client.py; a signed query gets 401)
        let sign_path = path.split('?').next().unwrap_or(path);
        let sig = self.sign(&ts, "GET", sign_path);
        self.agent
            .get(&format!("https://api.elections.kalshi.com{path}"))
            .set("KALSHI-ACCESS-KEY", &self.key_id)
            .set("KALSHI-ACCESS-SIGNATURE", &sig)
            .set("KALSHI-ACCESS-TIMESTAMP", &ts)
            .set("Accept", "application/json")
            .call()
            .map_err(|e| e.to_string())?
            .into_json::<serde_json::Value>()
            .map_err(|e| e.to_string())
    }

    fn post(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis()
            .to_string();
        let sig = self.sign(&ts, "POST", path);
        self.agent
            .post(&format!("https://api.elections.kalshi.com{path}"))
            .set("KALSHI-ACCESS-KEY", &self.key_id)
            .set("KALSHI-ACCESS-SIGNATURE", &sig)
            .set("KALSHI-ACCESS-TIMESTAMP", &ts)
            .set("Accept", "application/json")
            .set("Content-Type", "application/json")
            .send_json(body.clone())
            .map_err(|e| e.to_string())?
            .into_json::<serde_json::Value>()
            .map_err(|e| e.to_string())
    }

    /// Read the authoritative shard for one market ticker. This is the
    /// tier-1 truth: order placement MUST call this, never trust a hint.
    pub fn market_shard(&self, ticker: &str) -> Result<MarketShard, String> {
        let v = self.get(&format!("/trade-api/v2/markets/{ticker}"))?;
        let m = &v["market"];
        let idx = m["exchange_index"].as_i64().ok_or("missing exchange_index")?;
        let shard = Shard::from_index(idx).map_err(|e| e.to_string())?;
        Ok(MarketShard {
            shard,
            status: m["status"].as_str().unwrap_or("").to_string(),
        })
    }

    /// Per-shard balances, in exact centicents.
    pub fn balances(&self) -> Result<std::collections::HashMap<Shard, Centicents>, String> {
        let v = self.get("/trade-api/v2/portfolio/balance")?;
        let mut out = std::collections::HashMap::new();
        for row in v["balance_breakdown"].as_array().ok_or("no balance_breakdown")? {
            let idx = row["exchange_index"].as_i64().ok_or("missing shard")?;
            let bal = row["balance"].as_str().ok_or("missing balance")?;
            let shard = Shard::from_index(idx).map_err(|e| e.to_string())?;
            out.insert(shard, Centicents::parse_dollars(bal).map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Open positions: (ticker, position in contracts — negative = short YES
    /// i.e. long NO). Account-wide across all shards.
    pub fn positions(&self) -> Result<Vec<(String, i64)>, String> {
        let v = self.get("/trade-api/v2/portfolio/positions?limit=200&count_filter=position")?;
        let mut out = Vec::new();
        for p in v["market_positions"].as_array().ok_or("no market_positions")? {
            let ticker = p["ticker"].as_str().ok_or("position missing ticker")?;
            let pos = p["position_fp"].as_str().unwrap_or("0");
            let n: f64 = pos.parse().map_err(|e: std::num::ParseFloatError| e.to_string())?;
            // positions are contract counts — integral by construction
            out.push((ticker.to_string(), n as i64));
        }
        Ok(out)
    }

    /// Resting orders: (ticker, count in contracts).
    pub fn resting_orders(&self) -> Result<Vec<(String, i64)>, String> {
        let v = self.get("/trade-api/v2/portfolio/orders?status=resting&limit=100")?;
        let mut out = Vec::new();
        for o in v["orders"].as_array().ok_or("no orders")? {
            let ticker = o["ticker"].as_str().ok_or("order missing ticker")?;
            let cnt = o["original_count"].as_str().or_else(|| o["remaining_count"].as_str()).unwrap_or("0");
            let n: f64 = cnt.parse().map_err(|e: std::num::ParseFloatError| e.to_string())?;
            out.push((ticker.to_string(), n as i64));
        }
        Ok(out)
    }

    /// Fee schedule for a series (fee_type + multiplier, exact).
    pub fn series_fees(&self, series: &str) -> Result<crate::router::FeeSchedule, String> {
        let v = self.get(&format!("/trade-api/v2/series/{series}"))?;
        let s = &v["series"];
        let fee_type = s["fee_type"].as_str().ok_or("missing fee_type")?;
        let mult = s["fee_multiplier"].as_f64().ok_or("missing fee_multiplier")?;
        crate::router::FeeSchedule::from_series_json(fee_type, mult)
    }

    /// Wait for a transfer to land: poll the destination shard's balance
    /// until it covers `need`, bounded by MAX_POLLS. Feeds observations
    /// through the SAME pure transition the DST harness exercises.
    pub fn wait_for_landed(
        &self,
        dest: Shard,
        need: Centicents,
    ) -> Result<crate::dst::WaitState, String> {
        let mut state = crate::dst::WaitState::Waiting { polls: 0, need };
        for _ in 0..crate::dst::MAX_POLLS {
            let bals = self.balances()?;
            let obs = bals.get(&dest).copied().unwrap_or(Centicents(0));
            state = crate::dst::transition(
                state,
                crate::dst::Event::Tick { dest_balance: obs },
            );
            if !matches!(state, crate::dst::WaitState::Waiting { .. }) {
                return Ok(state);
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        Ok(state)
    }

    /// Move cash between shards. Amount in centicents (the unit the exchange
    /// itself uses — a $15 transfer is 150_000, the bug we already paid once).
    pub fn transfer(&self, from: Shard, to: Shard, amount_centicents: i64) -> Result<String, String> {
        if from == to {
            return Err(RouterError::SelfTransfer.to_string());
        }
        let body = serde_json::json!({
            "source": "event_contract",
            "destination": "event_contract",
            "amount": amount_centicents,
            "source_exchange_shard": from.0,
            "destination_exchange_shard": to.0,
        });
        let v = self.post("/trade-api/v2/portfolio/intra_exchange_instance_transfer", &body)?;
        Ok(v["transfer_id"].as_str().ok_or("no transfer_id in response")?.to_string())
    }
}
