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
        let sig = self.sign(&ts, "GET", path);
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
