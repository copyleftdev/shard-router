//! dst.rs — deterministic simulation harness for the prefund-wait flow.
//!
//! Pattern (TigerBeetle VOPR): the core is a pure state machine
//! `transition(state, event) -> state`; ALL entropy (balance observations,
//! transfer landing tick) is injected as events; nothing reads the clock,
//! the network, or global state. The live adapter in kalshi.rs feeds real
//! observations through the same transition; tests feed PRNG-derived ones.
//! A failing seed is a coordinate, not a story.

use crate::router::Centicents;

/// Wires the DST harness in as a hidden command so the simulator is compiled
/// (and its invariants live) in every build, not only under `cargo test`.
pub fn sim_once(seed: u64) -> WaitState {
    simulate_prefund_wait(seed, 1_290_010, 150_000, 1_400_000, 7)
}

/// Events the state machine consumes. `Tick` carries the observed
/// destination-shard balance at that poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Tick { dest_balance: Centicents },
}

/// Hard upper bound on polls: "put a limit on everything" (TigerStyle).
pub const MAX_POLLS: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitState {
    /// Transfer accepted; waiting for the cash to actually land.
    Waiting { polls: u32, need: Centicents },
    /// Observed balance >= need. Fully landed.
    Landed { polls: u32 },
    /// Budget exhausted with the cash never observed. Bounded failure —
    /// the caller MUST NOT place orders against the assumed funds.
    Stuck { polls: u32, last_seen: Centicents },
}

/// The pure transition. Deterministic: same state + same event => same state.
/// Invariant: returns Landed ONLY on a tick whose observation covers `need`
/// — never before (pairing with the Kalshi adapter's post-transfer read).
pub fn transition(state: WaitState, event: Event) -> WaitState {
    match (&state, event) {
        (WaitState::Waiting { polls, need }, Event::Tick { dest_balance }) => {
            assert!(*polls < MAX_POLLS, "transition called past the poll budget");
            let polls = polls + 1;
            if dest_balance.as_i64() >= need.as_i64() {
                WaitState::Landed { polls }
            } else if polls >= MAX_POLLS {
                WaitState::Stuck { polls, last_seen: dest_balance }
            } else {
                WaitState::Waiting { polls, need: *need }
            }
        }
        (WaitState::Landed { .. }, _) => state,
        (WaitState::Stuck { .. }, _) => state,
    }
}

/// Seeded LCG PRNG — the single source of nondeterminism in simulation.
/// Reproducible by construction: same seed => same sequence.
pub struct Prng(u64);

impl Prng {
    pub fn new(seed: u64) -> Prng {
        Prng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        // Numerical Recipes 64-bit LCG — small, adequate for scenario shaping.
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }

    /// Simulate a balance observation: pre-landing balances stay near the
    /// starting level; `landing_tick` flips the cash in permanently.
    fn observe(&mut self, tick: u32, landing_tick: u32, start: i64, amount: i64) -> Centicents {
        if tick >= landing_tick {
            Centicents(start + amount)
        } else {
            // small deterministic jitter so pre-landing polls look real
            let jitter = (self.next_u64() % 100) as i64 - 50;
            Centicents((start + jitter).max(0))
        }
    }
}

/// Run one simulated prefund-wait scenario. Returns the terminal state.
/// The seed fully determines the world (TigerBeetle: "the seed is the handle").
pub fn simulate_prefund_wait(seed: u64, start_cc: i64, amount_cc: i64, need_cc: i64, landing_tick: u32) -> WaitState {
    let mut prng = Prng::new(seed);
    let mut state = WaitState::Waiting { polls: 0, need: Centicents(need_cc) };
    for tick in 0..MAX_POLLS {
        let obs = prng.observe(tick, landing_tick, start_cc, amount_cc);
        state = transition(state, Event::Tick { dest_balance: obs });
        match state {
            WaitState::Landed { .. } | WaitState::Stuck { .. } => break,
            WaitState::Waiting { .. } => {}
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_anchor_live_fill_86c_quadratic() {
        // Named regression: the live OKC fill (2026-09-29) paid exactly
        // $0.0084/contract taker at 86c on a quadratic mult-1 series.
        let sched = crate::router::FeeSchedule { maker_fees: false, multiplier_pm: 1000 };
        assert_eq!(sched.taker_fee_centicents(8600).unwrap(), 84);
        // max fee at 50c is 175cc = $0.0175
        assert_eq!(sched.taker_fee_centicents(5000).unwrap(), 175);
        // half multiplier at 50c: 87.5 -> 88 (half-up, never undercharge)
        let half = crate::router::FeeSchedule { maker_fees: false, multiplier_pm: 500 };
        assert_eq!(half.taker_fee_centicents(5000).unwrap(), 88);
    }

    #[test]
    fn fee_maker_zero_on_quadratic_lanes() {
        // Named regression: tennis challengers are fee_type quadratic —
        // maker fills cost zero (verified live: 5 maker fills, $0.000000 fees).
        let sched = crate::router::FeeSchedule { maker_fees: false, multiplier_pm: 1000 };
        assert_eq!(sched.maker_fee_centicents(5800).unwrap(), 0);
    }

    #[test]
    fn fee_maker_schedule_on_maker_fee_lanes() {
        // Published fee schedule: maker fee range $0.02–$0.44 per 100 ct
        // => max $0.0044/ct at 50c = 44 centicents.
        let sched = crate::router::FeeSchedule { maker_fees: true, multiplier_pm: 1000 };
        assert_eq!(sched.maker_fee_centicents(5000).unwrap(), 44);
        // 0.0176 * 0.14 * 0.86 = $0.00212 -> 21 centicents
        assert_eq!(sched.maker_fee_centicents(1400).unwrap(), 21);
    }

    #[test]
    fn prefund_lands_exactly_when_observed() {
        // DST: transfer lands at tick index 7 (0-based) -> Landed at poll 8,
        // the first observation covering `need`. Never earlier.
        let st = simulate_prefund_wait(0x9E3779B97F4A7C15, 1_290_010, 150_000, 1_400_000, 7);
        assert_eq!(st, WaitState::Landed { polls: 8 });
    }

    #[test]
    fn prefund_stuck_is_bounded_and_never_fake_lands() {
        // DST: cash never arrives -> Stuck at exactly MAX_POLLS, and the
        // last-seen balance is a pre-landing jittered observation (< need).
        let st = simulate_prefund_wait(0xDEADBEEF, 100_000, 1_400_000, 1_400_000, 1_000);
        match st {
            WaitState::Stuck { polls, last_seen } => {
                assert_eq!(polls, MAX_POLLS);
                assert!(last_seen.as_i64() < 1_400_000);
            }
            other => panic!("expected Stuck, got {other:?}"),
        }
    }

    #[test]
    fn prefund_deterministic_replay() {
        // DST core promise: same seed => identical terminal state and poll count.
        for seed in [1u64, 42, 0xABCDEF, u64::MAX] {
            let a = simulate_prefund_wait(seed, 50_000, 900_000, 900_000, 13);
            let b = simulate_prefund_wait(seed, 50_000, 900_000, 900_000, 13);
            assert_eq!(a, b);
        }
    }

    #[test]
    fn prefund_invariant_landed_only_after_observation() {
        // Positive and negative space (TigerStyle golden rule): across many
        // seeds, Landed poll count >= landing tick, never less.
        for seed in 0..64u64 {
            let tick = (seed % 30) as u32;
            let st = simulate_prefund_wait(seed, 0, 1_000_000, 1_000_000, tick);
            match st {
                WaitState::Landed { polls } => assert!(polls >= tick + 1),
                WaitState::Stuck { .. } => assert!(tick >= MAX_POLLS),
                WaitState::Waiting { .. } => panic!("terminal states only"),
            }
        }
    }
}
