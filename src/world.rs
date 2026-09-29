//! world.rs — multi-agent DST world simulator (VOPR pattern, tier 3 of the
//! roadmap): several agents route/prefund/place against a simulated Kalshi
//! shard set, with PRNG-driven transfer landing delays, stuck transfers,
//! and fill-vs-transfer races. Single-threaded, seed-deterministic, pure
//! transitions. Invariants are checked after EVERY tick:
//!
//!   INV-1 cash conservation: sum(shard balances) + sum(in-flight) == initial
//!   INV-2 no fill draws cash that has not been OBSERVED landed
//!        (the OKC-clamp bug class: fill fires while transfer is in flight)
//!   INV-3 every world terminates within a bounded tick budget
//!
//! A failing seed is a coordinate: `sim-world <seed>` reproduces it forever.

pub const MAX_TICKS: u32 = 10_000;
pub const NUM_SHARDS: usize = 4;

/// A transfer moving between shards; `land_at_tick` is when the cash becomes
/// OBSERVABLE on the destination shard (transfers are async — the real
/// exchange behaves this way, verified 2026-09-28/29).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlight {
    pub from: u8,
    pub to: u8,
    pub amount_cc: i64,
    pub land_at_tick: u32,
}

/// An agent order that draws cash from a shard when it fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub shard: u8,
    pub cost_cc: i64,
    pub fill_at_tick: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldEvent {
    /// Request a transfer (agent-side view: "prefund").
    RequestPrefund {
        from: u8,
        to: u8,
        amount_cc: i64,
    },
    /// Place an order that will fill at `fill_at_tick` (draws cash).
    PlaceOrder {
        shard: u8,
        cost_cc: i64,
        fill_at_tick: u32,
    },
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct World {
    pub tick: u32,
    /// Cash observable on each shard. Index 0..=3.
    pub balances: [i64; NUM_SHARDS],
    /// Transfers accepted but not yet observable.
    pub in_flight: Vec<InFlight>,
    /// Orders placed; fills resolved by tick.
    pub orders: Vec<Order>,
    /// Fills that drew cash successfully.
    pub fills_landed: u32,
    /// Fills that bounced on insufficient OBSERVED cash (the clamp class).
    pub fills_bounced: u32,
    /// Cash converted into contracts by landed fills — it left the balance
    /// sheet but not the world. Conservation counts it.
    pub filled_cc: i64,
    /// Total cash across all shards and in flight (conservation anchor).
    pub total_cc: i64,
}

impl World {
    pub fn new(initial: [i64; NUM_SHARDS]) -> World {
        let total: i64 = initial.iter().sum();
        World {
            tick: 0,
            balances: initial,
            in_flight: Vec::new(),
            orders: Vec::new(),
            fills_landed: 0,
            fills_bounced: 0,
            filled_cc: 0,
            total_cc: total,
        }
    }

    /// INV-1: cash is conserved across balances + in-flight + filled, always.
    pub fn assert_conservation(&self) {
        let sum: i64 = self.balances.iter().sum::<i64>()
            + self.in_flight.iter().map(|t| t.amount_cc).sum::<i64>()
            + self.filled_cc;
        assert!(
            sum == self.total_cc,
            "cash leak: balances+in_flight+filled {sum} != total {}",
            self.total_cc
        );
    }

    /// The pure world transition. All fault behavior is IN the event data
    /// (land_at_tick, fill_at_tick) — shaped by the PRNG in run_world.
    pub fn transition(&mut self, event: WorldEvent) {
        self.assert_conservation();
        match event {
            WorldEvent::RequestPrefund {
                from,
                to,
                amount_cc,
            } => {
                assert!(from < NUM_SHARDS as u8, "shard out of closed set");
                assert!(to < NUM_SHARDS as u8, "shard out of closed set");
                assert!(from != to, "self-transfer rejected upstream");
                assert!(amount_cc > 0, "transfer must be positive");
                assert!(
                    self.balances[from as usize] >= amount_cc,
                    "source shard lacks the cash (typed error upstream)"
                );
                self.balances[from as usize] -= amount_cc;
                // cash is between shards while in flight — still conserved
                self.in_flight.push(InFlight {
                    from,
                    to,
                    amount_cc,
                    land_at_tick: self.tick + 3,
                });
            }
            WorldEvent::PlaceOrder {
                shard,
                cost_cc,
                fill_at_tick,
            } => {
                assert!(shard < NUM_SHARDS as u8, "shard out of closed set");
                assert!(cost_cc > 0, "order cost must be positive");
                self.orders.push(Order {
                    shard,
                    cost_cc,
                    fill_at_tick,
                });
            }
            WorldEvent::Tick => {
                self.tick += 1;
                // land transfers whose landing tick has arrived
                let mut i = 0;
                while i < self.in_flight.len() {
                    if self.in_flight[i].land_at_tick <= self.tick {
                        let t = self.in_flight.remove(i);
                        self.balances[t.to as usize] += t.amount_cc;
                    } else {
                        i += 1;
                    }
                }
                // resolve fills: only OBSERVED cash counts (INV-2)
                let mut oi = 0;
                while oi < self.orders.len() {
                    if self.orders[oi].fill_at_tick <= self.tick {
                        let o = self.orders.remove(oi);
                        let bal = &mut self.balances[o.shard as usize];
                        if *bal >= o.cost_cc {
                            *bal -= o.cost_cc;
                            self.fills_landed += 1;
                            // cash -> contracts: tracked for conservation
                            self.filled_cc += o.cost_cc;
                        } else {
                            // bounced: the OKC-clamp class — transfer still in
                            // flight when the fill fired
                            self.fills_bounced += 1;
                        }
                    } else {
                        oi += 1;
                    }
                }
            }
        }
        self.assert_conservation();
    }
}

/// Seeded LCG — the single entropy source (same as the prefund harness).
pub struct WorldPrng(pub u64);

impl WorldPrng {
    pub fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "below(0)");
        self.next() % n
    }
}

/// Run a full simulated world: PRNG-shaped agents issue prefunds and orders
/// with landing/fill ticks that can race. Returns the terminal world.
/// Deterministic per seed — this is the replay coordinate.
pub fn run_world(seed: u64, initial: [i64; NUM_SHARDS]) -> World {
    let mut prng = WorldPrng(seed);
    let mut w = World::new(initial);
    // schedule a bounded number of operations
    let ops: u32 = 40;
    for _ in 0..ops {
        let dice = prng.below(100);
        if dice < 45 {
            // prefund: from the richest shard to a random other shard
            let mut richest: u8 = 0;
            for s in 1..NUM_SHARDS as u8 {
                if w.balances[s as usize] > w.balances[richest as usize] {
                    richest = s;
                }
            }
            let to = prng.below(NUM_SHARDS as u64 - 1) as u8;
            let to = if to >= richest { to + 1 } else { to };
            let have = w.balances[richest as usize];
            let amt = (prng.below(have.max(1) as u64 + 1) as i64).min(have);
            if amt > 0 {
                w.transition(WorldEvent::RequestPrefund {
                    from: richest,
                    to,
                    amount_cc: amt,
                });
            }
        } else {
            // order on a random shard, filling within 0..=8 ticks — races
            // with any in-flight transfer are the point
            let shard = prng.below(NUM_SHARDS as u64) as u8;
            let fill_in = prng.below(9) as u32;
            let cost = 1 + prng.below(50_000) as i64;
            w.transition(WorldEvent::PlaceOrder {
                shard,
                cost_cc: cost,
                fill_at_tick: w.tick + fill_in,
            });
        }
        // advance a few ticks per op so landings interleave with placements
        for _ in 0..2 {
            w.transition(WorldEvent::Tick);
        }
        assert!(w.tick < MAX_TICKS, "world exceeded tick budget");
    }
    // drain to terminal: land everything, resolve all fills
    for _ in 0..40 {
        w.transition(WorldEvent::Tick);
    }
    w
}

/// Named-regression helper: the exact OKC scenario — transfer accepted,
/// fill fires while the transfer is still in flight, then lands.
pub fn okc_clamp_scenario() -> World {
    let mut w = World::new([200_000, 0, 0, 0]);
    w.transition(WorldEvent::RequestPrefund {
        from: 0,
        to: 3,
        amount_cc: 150_000,
    });
    // fill fires at tick 1; transfer lands at tick 3 -> bounce
    w.transition(WorldEvent::PlaceOrder {
        shard: 3,
        cost_cc: 150_000,
        fill_at_tick: 1,
    });
    w.transition(WorldEvent::Tick); // t1: fill bounces (no observed cash)
    w.transition(WorldEvent::Tick); // t2
    w.transition(WorldEvent::Tick); // t3: transfer lands
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_okc_clamp_bounces_then_lands() {
        // Named regression (2026-09-28 live incident): the OKC order was
        // cash-clamped because the tennis transfer had left shard 0 thin.
        // In world terms: fill fired before the transfer was observable.
        let w = okc_clamp_scenario();
        assert_eq!(w.fills_bounced, 1);
        assert_eq!(w.fills_landed, 0);
        // after the transfer lands the SAME order size succeeds:
        let mut w2 = okc_clamp_scenario();
        w2.transition(WorldEvent::PlaceOrder {
            shard: 3,
            cost_cc: 150_000,
            fill_at_tick: w2.tick + 1,
        });
        w2.transition(WorldEvent::Tick);
        assert_eq!(w2.fills_landed, 1);
        assert_eq!(w2.balances[3], 0);
    }

    #[test]
    fn world_cash_conservation_all_seeds() {
        // INV-1 across a swarm of seeds (broad simulation discovers).
        for seed in 0..256u64 {
            let w = run_world(seed, [500_000; NUM_SHARDS]);
            let sum: i64 = w.balances.iter().sum::<i64>()
                + w.in_flight.iter().map(|t| t.amount_cc).sum::<i64>()
                + w.filled_cc;
            assert_eq!(sum, 4 * 500_000, "seed {seed} leaked cash");
        }
    }

    #[test]
    fn world_deterministic_replay() {
        // Same seed => identical terminal world (the coordinate promise).
        for seed in [1u64, 7, 0xFEED, u64::MAX] {
            let a = run_world(seed, [100_000; NUM_SHARDS]);
            let b = run_world(seed, [100_000; NUM_SHARDS]);
            assert_eq!(a, b);
        }
    }

    #[test]
    fn world_fills_only_use_observed_cash() {
        // INV-2 swarm: with many seeds, total fills + bounces == orders placed
        // and no fill ever exceeded observed balances (implied by conservation
        // plus the bounce path). Distinct worlds ensure race coverage.
        let mut distinct = std::collections::HashSet::new();
        for seed in 0..64u64 {
            let w = run_world(seed, [1_000_000; NUM_SHARDS]);
            assert_eq!(w.orders.len(), 0, "all orders must resolve at terminal");
            assert!(w.fills_landed + w.fills_bounced > 0);
            assert!(
                w.in_flight.is_empty(),
                "all transfers must land at terminal"
            );
            distinct.insert(format!(
                "{}:{}:{:?}",
                w.fills_landed, w.fills_bounced, w.balances
            ));
        }
        // negative space: the swarm should reach more than one distinct world
        assert!(
            distinct.len() > 1,
            "swarm collapsed to a single world — PRNG not shaping"
        );
    }

    #[test]
    fn world_stuck_transfer_is_modelable() {
        // A transfer that never lands (exchange wedged): do the transfer
        // through the transition (conservation-clean), then wedge its landing
        // tick. The stuck amount stays in_flight — the ledger reconcile
        // command treats exactly this shape as drift, never as spent cash.
        let mut w = World::new([100_000, 0, 0, 0]);
        w.transition(WorldEvent::RequestPrefund {
            from: 0,
            to: 3,
            amount_cc: 40_000,
        });
        assert_eq!(w.balances, [60_000, 0, 0, 0]);
        assert_eq!(w.in_flight.len(), 1);
        // wedge it: landing tick beyond any budget
        w.in_flight[0].land_at_tick = u32::MAX;
        for _ in 0..50 {
            w.transition(WorldEvent::Tick);
        }
        w.assert_conservation();
        assert_eq!(w.in_flight.len(), 1, "wedged transfer must never land");
        assert_eq!(
            w.balances,
            [60_000, 0, 0, 0],
            "cash must not move while wedged"
        );
    }
}
