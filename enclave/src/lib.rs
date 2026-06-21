//! Parclose confidential clearing — the deterministic uniform-price crossing rule (design §4).
//!
//! This is the logic the production enclave runs over the decrypted orders of a closed window.
//! It is pure, **integer-only**, and **order-independent**: identical sealed inputs yield an
//! identical [`ClearingResult`] (and therefore an identical `output_hash`), which is what makes
//! settlement reproducible (R10.6 / I-6). No floating point anywhere.
//!
//! The result it returns is exactly what `CrossingEngine.settle` consumes; by construction it
//! satisfies the engine's per-account escrow-sufficiency and value-conservation checks:
//! `Σ fund_spent == Σ fund_credit` and `Σ cash_spent == Σ cash_credit`.
//!
//! ## Rule (design §4)
//! - A **subscribe** (buy) trades at any price `≤ limit`; a **redeem** (sell) at any price
//!   `≥ limit`. At candidate price `p`: `D(p)` = Σ buy sizes with `limit ≥ p`, `S(p)` = Σ sell
//!   sizes with `limit ≤ p`, matched volume `V(p) = min(D, S)`.
//! - `P*`: among the distinct limit prices, take those that **maximize `V`**, then those that
//!   **minimize `|D − S|`**; if a set remains, take the **tick-rounded midpoint** (round-half-up)
//!   of the lowest and highest remaining candidate.
//! - At `P*` the heavier side is **rationed pro-rata by size**, rounded **down**; residual units
//!   are handed out one each in **ascending order id** (`H = ciphertext hash`) — neutral and
//!   deterministic. The lighter side fills fully.

use odra::casper_types::bytesrepr::ToBytes;
use odra::casper_types::U256;
use odra::prelude::Address;
use parclose_shared::{ClearingResult, Order, Settlement, SIDE_REDEEM, SIDE_SUBSCRIBE};

pub mod open;
pub use open::{open_window, OpenedWindow, SealedSubmission};

/// A decrypted order together with its on-chain identity — the ciphertext hash recorded by
/// `SealedOrderBook`. The id is the neutral tiebreak key `H` for residual allocation.
pub struct SubmittedOrder {
    pub order: Order,
    pub id: [u8; 32],
}

/// Computes the uniform clearing price and matched set for a window's orders (design §4).
///
/// `price_tick` is the price granularity; order limits are expected to be multiples of it, and
/// `P*` is always a multiple of it. Returns a `ClearingResult` with `price == 0` and no fills
/// when nothing crosses.
///
/// Panics only if `price_tick == 0` (a misconfiguration, not a runtime condition).
pub fn clear(window_id: u64, orders: &[SubmittedOrder], price_tick: u64) -> ClearingResult {
    assert!(price_tick > 0, "price tick must be positive");

    let buys: Vec<&SubmittedOrder> = orders
        .iter()
        .filter(|o| o.order.window_id == window_id && o.order.side == SIDE_SUBSCRIBE)
        .collect();
    let sells: Vec<&SubmittedOrder> = orders
        .iter()
        .filter(|o| o.order.window_id == window_id && o.order.side == SIDE_REDEEM)
        .collect();

    let mut candidates: Vec<u64> = orders
        .iter()
        .filter(|o| o.order.window_id == window_id)
        .map(|o| o.order.limit)
        .collect();
    candidates.sort_unstable();
    candidates.dedup();

    let p_star = match select_price(&buys, &sells, &candidates, price_tick) {
        Some(p) => p,
        None => return ClearingResult { window_id, price: 0, fills: Vec::new() },
    };

    // Recompute the executable book at the chosen price and ration each side to V.
    let eligible_buys: Vec<&SubmittedOrder> =
        buys.iter().copied().filter(|o| o.order.limit >= p_star).collect();
    let eligible_sells: Vec<&SubmittedOrder> =
        sells.iter().copied().filter(|o| o.order.limit <= p_star).collect();
    let buy_total = total_size(&eligible_buys);
    let sell_total = total_size(&eligible_sells);
    let v = min(buy_total, sell_total);
    if v.is_zero() {
        return ClearingResult { window_id, price: 0, fills: Vec::new() };
    }

    let buy_fills = ration(&eligible_buys, buy_total, v);
    let sell_fills = ration(&eligible_sells, sell_total, v);
    let price = U256::from(p_star);

    let mut acc: Vec<(Address, Settlement)> = Vec::new();
    for (o, q) in eligible_buys.iter().zip(buy_fills.iter()) {
        if q.is_zero() {
            continue;
        }
        // Buyer pays `q * P*` cash and receives `q` fund.
        add_fill(&mut acc, o.order.account, U256::zero(), *q * price, *q, U256::zero());
    }
    for (o, q) in eligible_sells.iter().zip(sell_fills.iter()) {
        if q.is_zero() {
            continue;
        }
        // Seller gives `q` fund and receives `q * P*` cash.
        add_fill(&mut acc, o.order.account, *q, U256::zero(), U256::zero(), *q * price);
    }

    // Canonical fills order: ascending by serialized account, independent of submission order.
    acc.sort_by(|a, b| account_key(&a.0).cmp(&account_key(&b.0)));
    let fills = acc.into_iter().map(|(_, s)| s).collect();

    ClearingResult { window_id, price: p_star, fills }
}

/// Selects `P*` per §4, or `None` if nothing crosses (`max V == 0`).
fn select_price(
    buys: &[&SubmittedOrder],
    sells: &[&SubmittedOrder],
    candidates: &[u64],
    tick: u64,
) -> Option<u64> {
    let mut max_v = U256::zero();
    for &c in candidates {
        let v = min(demand_at(buys, c), supply_at(sells, c));
        if v > max_v {
            max_v = v;
        }
    }
    if max_v.is_zero() {
        return None;
    }

    // Minimum imbalance among the max-volume candidates.
    let mut min_imb: Option<U256> = None;
    for &c in candidates {
        let d = demand_at(buys, c);
        let s = supply_at(sells, c);
        if min(d, s) == max_v {
            let imb = abs_diff(d, s);
            min_imb = Some(match min_imb {
                Some(m) if m <= imb => m,
                _ => imb,
            });
        }
    }
    let min_imb = min_imb.expect("max_v is nonzero, so at least one candidate qualifies");

    // The remaining tied set (max volume and minimum imbalance), in ascending price order.
    let tied: Vec<u64> = candidates
        .iter()
        .copied()
        .filter(|&c| {
            let d = demand_at(buys, c);
            let s = supply_at(sells, c);
            min(d, s) == max_v && abs_diff(d, s) == min_imb
        })
        .collect();
    let lo = *tied.first().expect("tied set is non-empty");
    let hi = *tied.last().expect("tied set is non-empty");
    Some(round_half_up_to_tick(lo, hi, tick))
}

/// `round_half_up( midpoint(lo, hi) / tick ) * tick`, computed in u128 to avoid overflow.
fn round_half_up_to_tick(lo: u64, hi: u64, tick: u64) -> u64 {
    let sum = lo as u128 + hi as u128; // == 2 * midpoint
    let t = tick as u128;
    let units = (sum + t) / (2 * t); // round-half-up of midpoint/tick
    (units * t) as u64
}

/// Rations `orders` to a total of `v`, pro-rata by size (floored), distributing the rounding
/// residual one unit each in ascending order id. When `total == v` every order fills fully.
fn ration(orders: &[&SubmittedOrder], total: U256, v: U256) -> Vec<U256> {
    let n = orders.len();
    if n == 0 || v.is_zero() || total.is_zero() {
        return vec![U256::zero(); n];
    }
    let mut fills: Vec<U256> = orders.iter().map(|o| o.order.size * v / total).collect();
    let allocated = fills.iter().fold(U256::zero(), |a, b| a + *b);
    let mut residual = v - allocated; // 0 <= residual < n

    if !residual.is_zero() {
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by(|&a, &b| orders[a].id.cmp(&orders[b].id));
        let one = U256::from(1u64);
        for &i in &idx {
            if residual.is_zero() {
                break;
            }
            fills[i] += one;
            residual -= one;
        }
    }
    fills
}

fn add_fill(
    acc: &mut Vec<(Address, Settlement)>,
    account: Address,
    fund_spent: U256,
    cash_spent: U256,
    fund_credit: U256,
    cash_credit: U256,
) {
    for entry in acc.iter_mut() {
        if entry.0 == account {
            entry.1.fund_spent += fund_spent;
            entry.1.cash_spent += cash_spent;
            entry.1.fund_credit += fund_credit;
            entry.1.cash_credit += cash_credit;
            return;
        }
    }
    acc.push((account, Settlement { account, fund_spent, cash_spent, fund_credit, cash_credit }));
}

fn demand_at(buys: &[&SubmittedOrder], p: u64) -> U256 {
    buys.iter()
        .filter(|o| o.order.limit >= p)
        .fold(U256::zero(), |a, o| a + o.order.size)
}

fn supply_at(sells: &[&SubmittedOrder], p: u64) -> U256 {
    sells
        .iter()
        .filter(|o| o.order.limit <= p)
        .fold(U256::zero(), |a, o| a + o.order.size)
}

fn total_size(orders: &[&SubmittedOrder]) -> U256 {
    orders.iter().fold(U256::zero(), |a, o| a + o.order.size)
}

fn min(a: U256, b: U256) -> U256 {
    if a <= b {
        a
    } else {
        b
    }
}

fn abs_diff(a: U256, b: U256) -> U256 {
    if a >= b {
        a - b
    } else {
        b - a
    }
}

fn account_key(a: &Address) -> Vec<u8> {
    a.to_bytes().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use odra::casper_types::account::AccountHash;

    const TICK: u64 = 1;

    fn addr(n: u8) -> Address {
        Address::Account(AccountHash::new([n; 32]))
    }

    fn order(side: u8, size: u64, limit: u64, account: u8) -> SubmittedOrder {
        SubmittedOrder {
            order: Order {
                side,
                size: U256::from(size),
                limit,
                window_id: 1,
                account: addr(account),
            },
            // id == account byte here, so "ascending id" == ascending account number in tests.
            id: [account; 32],
        }
    }

    fn buy(size: u64, limit: u64, account: u8) -> SubmittedOrder {
        order(SIDE_SUBSCRIBE, size, limit, account)
    }

    fn sell(size: u64, limit: u64, account: u8) -> SubmittedOrder {
        order(SIDE_REDEEM, size, limit, account)
    }

    /// Returns (Σfund_spent, Σfund_credit, Σcash_spent, Σcash_credit).
    fn totals(r: &ClearingResult) -> (U256, U256, U256, U256) {
        let mut fs = U256::zero();
        let mut fc = U256::zero();
        let mut cs = U256::zero();
        let mut cc = U256::zero();
        for f in &r.fills {
            fs += f.fund_spent;
            fc += f.fund_credit;
            cs += f.cash_spent;
            cc += f.cash_credit;
        }
        (fs, fc, cs, cc)
    }

    fn assert_conserves(r: &ClearingResult) {
        let (fs, fc, cs, cc) = totals(r);
        assert_eq!(fs, fc, "fund spent must equal fund credited");
        assert_eq!(cs, cc, "cash spent must equal cash credited");
    }

    #[test]
    fn balanced_cross_fills_both_fully() {
        // D(100)=200, S(100)=200 → max volume at P*=100; both sides fill fully.
        let orders = vec![
            buy(100, 105, 1),
            buy(100, 100, 2),
            sell(100, 95, 3),
            sell(100, 100, 4),
        ];
        let r = clear(1, &orders, TICK);
        assert_eq!(r.price, 100);
        assert_eq!(r.fills.len(), 4);
        let (fs, fc, cs, cc) = totals(&r);
        assert_eq!(fs, U256::from(200u64));
        assert_eq!(fc, U256::from(200u64));
        assert_eq!(cs, U256::from(20_000u64));
        assert_eq!(cc, U256::from(20_000u64));
        assert_conserves(&r);
    }

    #[test]
    fn lighter_side_fills_fully_heavier_rationed() {
        // Buys 200 vs sells 150 at P*=100: sells fill fully, buys rationed pro-rata to 150.
        let orders = vec![buy(100, 100, 1), buy(100, 100, 2), sell(150, 100, 3)];
        let r = clear(1, &orders, TICK);
        assert_eq!(r.price, 100);
        assert_conserves(&r);
        // Each buyer: floor(100 * 150 / 200) = 75.
        let b1 = r.fills.iter().find(|f| f.account == addr(1)).unwrap();
        let b2 = r.fills.iter().find(|f| f.account == addr(2)).unwrap();
        let s3 = r.fills.iter().find(|f| f.account == addr(3)).unwrap();
        assert_eq!(b1.fund_credit, U256::from(75u64));
        assert_eq!(b2.fund_credit, U256::from(75u64));
        assert_eq!(s3.fund_spent, U256::from(150u64));
    }

    #[test]
    fn residual_allocated_by_ascending_id() {
        // Buys 300 vs sells 100: each buy floors to 33 (sum 99), residual 1 → smallest id (#1).
        let orders = vec![
            buy(100, 100, 1),
            buy(100, 100, 2),
            buy(100, 100, 3),
            sell(100, 100, 9),
        ];
        let r = clear(1, &orders, TICK);
        assert_conserves(&r);
        let b1 = r.fills.iter().find(|f| f.account == addr(1)).unwrap();
        let b2 = r.fills.iter().find(|f| f.account == addr(2)).unwrap();
        let b3 = r.fills.iter().find(|f| f.account == addr(3)).unwrap();
        assert_eq!(b1.fund_credit, U256::from(34u64)); // residual unit to the smallest id
        assert_eq!(b2.fund_credit, U256::from(33u64));
        assert_eq!(b3.fund_credit, U256::from(33u64));
        // Total matched == 100.
        assert_eq!(b1.fund_credit + b2.fund_credit + b3.fund_credit, U256::from(100u64));
    }

    #[test]
    fn picks_max_volume_price() {
        // Volume is maximized at p=100 (200) over p=95 (100) and p=105 (100).
        let orders = vec![
            buy(100, 105, 1),
            buy(100, 100, 2),
            sell(100, 95, 3),
            sell(100, 100, 4),
        ];
        let r = clear(1, &orders, TICK);
        assert_eq!(r.price, 100);
    }

    #[test]
    fn tie_break_is_tick_rounded_midpoint() {
        // Two single orders that cross over a price range; the tied max-volume candidates are
        // {95, 105}; midpoint 100 rounds (tick 10) to 100.
        let orders = vec![buy(100, 105, 1), sell(100, 95, 2)];
        let r = clear(1, &orders, 10);
        // D and S are both 100 at p in {95,105}; V=100 at both, imbalance 0 at both → tied {95,105}.
        assert_eq!(r.price, 100);
        assert_conserves(&r);
    }

    #[test]
    fn no_cross_returns_empty() {
        // Highest bid (90) below lowest ask (100): nothing crosses.
        let orders = vec![buy(100, 90, 1), sell(100, 100, 2)];
        let r = clear(1, &orders, TICK);
        assert_eq!(r.price, 0);
        assert!(r.fills.is_empty());
    }

    #[test]
    fn only_one_side_returns_empty() {
        let orders = vec![buy(100, 100, 1), buy(50, 99, 2)];
        let r = clear(1, &orders, TICK);
        assert_eq!(r.price, 0);
        assert!(r.fills.is_empty());
    }

    #[test]
    fn orders_for_other_windows_are_ignored() {
        let mut foreign = buy(1_000, 100, 5);
        foreign.order.window_id = 2; // different window
        let orders = vec![buy(100, 100, 1), sell(100, 100, 2), foreign];
        let r = clear(1, &orders, TICK);
        assert_eq!(r.price, 100);
        // The foreign account must not appear in the fills.
        assert!(r.fills.iter().all(|f| f.account != addr(5)));
        assert_conserves(&r);
    }

    #[test]
    fn order_independent() {
        let mut a = vec![
            buy(120, 101, 1),
            buy(80, 100, 2),
            sell(60, 99, 3),
            sell(150, 100, 4),
        ];
        let r1 = clear(1, &a, TICK);
        a.reverse();
        let r2 = clear(1, &a, TICK);
        // The committed bytes (what output_hash binds) must be identical regardless of input order.
        assert_eq!(r1.to_bytes().unwrap(), r2.to_bytes().unwrap());
        assert_conserves(&r1);
    }
}
