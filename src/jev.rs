//! jev.rs — semantic tier: TypeSafe Jev System One, used ONLY where the API
//! has no answer (unknown series, free-text market ideas). A jev verdict is
//! a HINT with a confidence gate; the order path re-verifies via the API.
//!
//! Pitfalls encoded (verified 2026-09-20/26): atomic questions, never
//! composite mush; one state per call; confidence is the act/not-act axis.

use crate::router::{Shard, Verdict};

pub struct Jev {
    api_key: String,
    agent: ureq::Agent,
}

pub const CONFIDENCE_GATE: f64 = 0.80;

impl Jev {
    pub fn from_env() -> Result<Jev, String> {
        let path = "/home/ops/.creds/typsafe_labs.env";
        let txt = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let key = txt
            .lines()
            .find_map(|l| l.strip_prefix("API_KEY="))
            .ok_or("API_KEY not found in typsafe_labs.env")?
            .trim()
            .to_string();
        Ok(Jev {
            api_key: key,
            agent: ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(30)).build(),
        })
    }

    /// Ask jev which shard family a market description belongs to.
    /// Choice over the four empirically-mapped families; returns a Hint
    /// verdict, confidence-gated at the call site (never Verified — the API
    /// is the only truth for that).
    pub fn classify_market(&self, description: &str) -> Result<Verdict, String> {
        let body = serde_json::json!({
            "state": format!(
                "Kalshi exchange has 4 parallel matching-engine shards. Shard 0 hosts US macro, weather (KXHIGH), major US sports (NFL, MLB, CPI, politics, earnings mention markets). Shard 1 hosts MVE crosscategory microcap multi-venue events. Shard 2 hosts crypto/commodity daily range markets (BTC, ETH, XRP, copper). Shard 3 hosts tennis and international sports markets (ATP, WTA, EPL, Bundesliga, WNBA totals). Market to classify: {description}"
            ),
            "model": "jev-latest",
            "questions": {
                "shard": {
                    "type": "choice",
                    "instructions": "Pick which Kalshi exchange shard (matching engine) this market belongs on, judged from its description and asset class.",
                    "criteria": {
                        "0": "US macro / weather / major US sports / politics",
                        "1": "MVE crosscategory microcap events",
                        "2": "crypto or commodity daily range",
                        "3": "tennis or international sports"
                    }
                }
            }
        });
        let v: serde_json::Value = self
            .agent
            .post("https://api.typesafe.ai/v1/systemone")
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())?;
        let ans = &v["answers"]["shard"];
        let choice = ans["choice"].as_str().ok_or("jev returned no choice")?;
        let shard = Shard::from_index(choice.parse::<i64>().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let confidence = ans["confidence"].as_f64().unwrap_or(0.0);
        let hint = Verdict::Hint { shard, confidence };
        Ok(Jev::gated(hint))
    }

    /// A hint below the gate must be surfaced as Unknown, not routed.
    pub fn gated(verdict: Verdict) -> Verdict {
        match verdict {
            Verdict::Hint { shard, confidence } if confidence >= CONFIDENCE_GATE => {
                Verdict::Hint { shard, confidence }
            }
            _ => Verdict::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_confidence_hints_are_gated_to_unknown() {
        let low = Verdict::Hint { shard: Shard::THREE, confidence: 0.5 };
        assert_eq!(Jev::gated(low), Verdict::Unknown);
        let high = Verdict::Hint { shard: Shard::THREE, confidence: 0.95 };
        assert_eq!(Jev::gated(high), Verdict::Hint { shard: Shard::THREE, confidence: 0.95 });
    }
}
