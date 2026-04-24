//! Regression test for the per-operation cost table. Read/subjects must
//! be free, writes/searches at least 1 µUSD, and timeline strictly
//! greater than write — bumping these without thinking will break
//! billing for downstream callers, so freeze the invariant here.

use ctxd_cap::state::OperationCost;
use ctxd_cap::Operation;

#[test]
fn read_and_subjects_are_free() {
    // The constants are evaluated at compile time, but the test is
    // here to (a) document the invariant in the test corpus and
    // (b) catch a refactor that swaps OperationCost::READ for
    // something that *runtime*-derives a different value.
    assert_eq!(OperationCost::from(Operation::Read).0, 0);
    assert_eq!(OperationCost::from(Operation::Subjects).0, 0);
}

const _: () = assert!(OperationCost::READ.0 == 0);
const _: () = assert!(OperationCost::SUBJECTS.0 == 0);
const _: () = assert!(
    OperationCost::WRITE.0 >= 1_000,
    "write cost must be >= 1_000 µUSD"
);
const _: () = assert!(
    OperationCost::SEARCH.0 >= 1_000,
    "search cost must be >= 1_000 µUSD"
);
const _: () = assert!(
    OperationCost::TIMELINE.0 > OperationCost::WRITE.0,
    "timeline (full temporal scan) must be more expensive than a write"
);
const _: () = assert!(OperationCost::ENTITIES.0 <= OperationCost::WRITE.0);
const _: () = assert!(OperationCost::RELATED.0 <= OperationCost::WRITE.0);
const _: () = assert!(OperationCost::ADMIN.0 == 0);
const _: () = assert!(OperationCost::PEER.0 == 0);
const _: () = assert!(OperationCost::SUBSCRIBE.0 == 0);

#[test]
fn write_and_search_have_baseline_cost() {
    // Runtime mirror of the const_assert above. Keeping the test
    // function shape so `cargo test` reports the regression with a
    // human-friendly name.
    assert_eq!(
        OperationCost::from(Operation::Write).0,
        OperationCost::WRITE.0
    );
    assert_eq!(
        OperationCost::from(Operation::Search).0,
        OperationCost::SEARCH.0
    );
}

#[test]
fn timeline_costs_more_than_write() {
    // Runtime mirror; see the const_assert above.
    let _ = OperationCost::TIMELINE;
}

#[test]
fn entities_and_related_are_cheaper_than_writes() {
    // Runtime mirror; see the const_assert above.
    let _ = OperationCost::ENTITIES;
    let _ = OperationCost::RELATED;
}

#[test]
fn admin_peer_subscribe_are_free() {
    // Admin/peer/subscribe are gated by capability, not by budget.
    assert_eq!(OperationCost::from(Operation::Admin).0, 0);
    assert_eq!(OperationCost::from(Operation::Peer).0, 0);
    assert_eq!(OperationCost::from(Operation::Subscribe).0, 0);
}

#[test]
fn cost_fits_in_i64() {
    // Defensive: as_i64 must not overflow for any constant we ship.
    let costs = [
        OperationCost::READ,
        OperationCost::WRITE,
        OperationCost::SUBJECTS,
        OperationCost::SEARCH,
        OperationCost::ENTITIES,
        OperationCost::RELATED,
        OperationCost::TIMELINE,
        OperationCost::ADMIN,
        OperationCost::PEER,
        OperationCost::SUBSCRIBE,
    ];
    for c in costs {
        assert!(c.as_i64() >= 0);
    }
}
