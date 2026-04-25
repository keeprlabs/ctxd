//! Spike test for third-party block attenuation chains.
//!
//! Demonstrates the full A → B → C → presenter flow:
//! - A (root) mints a broad capability.
//! - B attenuates with a third-party block scoped to its own pubkey,
//!   restricting the subject prefix.
//! - C attenuates the result with another third-party block, narrowing
//!   the operation set.
//! - The final holder presents the token; the verifier must know all
//!   three pubkeys (root from `CapEngine`, plus B's and C's) to accept.
//!
//! Adversarial cases asserted in the same file:
//! - Wrong authority key in trust set: rejected.
//! - Missing intermediate authority key: rejected (loud fail).
//! - Widening attempt: rejected (we re-test outside the new scope).
//! - Expired chain: rejected.

use chrono::Duration;
use ctxd_cap::{BiscuitKeyPair, CapEngine, Caveat, Operation};

#[test]
fn three_authority_chain_end_to_end() {
    // A: root capability authority.
    let engine_a = CapEngine::new();
    let kp_b = BiscuitKeyPair::new();
    let kp_c = BiscuitKeyPair::new();

    // A mints a broad cap: read+write on /**.
    let token_a = engine_a
        .mint(
            "/**",
            &[Operation::Read, Operation::Write],
            None,
            None,
            None,
        )
        .expect("mint");

    // B attenuates: subject prefix /work and op restriction to read+write.
    let token_b = engine_a
        .attenuate_with_block(
            &token_a,
            &kp_b.private(),
            &[
                Caveat::SubjectPrefix("/work/**".to_string()),
                Caveat::OperationsAtMost(vec![Operation::Read, Operation::Write]),
            ],
        )
        .expect("B attenuates");

    // C attenuates: narrow further to /work/team1 and read-only.
    let token_c = engine_a
        .attenuate_with_block(
            &token_b,
            &kp_c.private(),
            &[
                Caveat::SubjectPrefix("/work/team1/**".to_string()),
                Caveat::OperationsAtMost(vec![Operation::Read]),
            ],
        )
        .expect("C attenuates");

    // Verifier knows all three: root (via engine_a), B, C.
    let trust = vec![kp_b.public(), kp_c.public()];

    // /work/team1/doc + read should succeed.
    engine_a
        .verify_multi(&token_c, &trust, "/work/team1/doc", Operation::Read)
        .expect("read should be allowed under full trust");
}

#[test]
fn wrong_authority_key_in_trust_set_is_rejected() {
    let engine = CapEngine::new();
    let kp_b = BiscuitKeyPair::new();
    let kp_c = BiscuitKeyPair::new();
    let kp_unrelated = BiscuitKeyPair::new();

    let token = engine
        .mint("/**", &[Operation::Read], None, None, None)
        .expect("mint");
    let token_b = engine
        .attenuate_with_block(
            &token,
            &kp_b.private(),
            &[Caveat::SubjectPrefix("/work/**".to_string())],
        )
        .expect("B");
    let token_c = engine
        .attenuate_with_block(
            &token_b,
            &kp_c.private(),
            &[Caveat::OperationsAtMost(vec![Operation::Read])],
        )
        .expect("C");

    // Trust set names *unrelated* + C — B is missing. The verification
    // must fail because B's check `resource starts_with "/work"` is
    // unverified (B is not in trust set), so the chain authorization
    // can't be satisfied for /work/foo.
    let bad_trust = vec![kp_unrelated.public(), kp_c.public()];
    let result = engine.verify_multi(&token_c, &bad_trust, "/work/foo", Operation::Read);
    assert!(
        result.is_err(),
        "verification must fail when an intermediate authority is wrong"
    );
}

#[test]
fn missing_intermediate_authority_is_rejected() {
    let engine = CapEngine::new();
    let kp_b = BiscuitKeyPair::new();
    let kp_c = BiscuitKeyPair::new();

    let token = engine
        .mint("/**", &[Operation::Read], None, None, None)
        .expect("mint");
    let token_b = engine
        .attenuate_with_block(
            &token,
            &kp_b.private(),
            &[Caveat::SubjectPrefix("/work/**".to_string())],
        )
        .expect("B");
    let token_c = engine
        .attenuate_with_block(
            &token_b,
            &kp_c.private(),
            &[Caveat::OperationsAtMost(vec![Operation::Read])],
        )
        .expect("C");

    // Trust set only has C; B is missing entirely.
    let trust_missing_b = vec![kp_c.public()];
    let result = engine.verify_multi(&token_c, &trust_missing_b, "/work/x", Operation::Read);
    assert!(
        result.is_err(),
        "verification must loud-fail when an intermediate authority is absent"
    );
}

#[test]
fn widening_attempt_outside_b_scope_is_rejected() {
    let engine = CapEngine::new();
    let kp_b = BiscuitKeyPair::new();

    let token = engine
        .mint(
            "/**",
            &[Operation::Read, Operation::Write],
            None,
            None,
            None,
        )
        .expect("mint");
    let token_b = engine
        .attenuate_with_block(
            &token,
            &kp_b.private(),
            &[Caveat::SubjectPrefix("/work/**".to_string())],
        )
        .expect("B");

    let trust = vec![kp_b.public()];

    // Inside scope: should pass.
    engine
        .verify_multi(&token_b, &trust, "/work/team1/doc", Operation::Read)
        .expect("inside scope ok");

    // Outside scope: must fail. B narrowed to /work/**.
    let result = engine.verify_multi(&token_b, &trust, "/home/x", Operation::Read);
    assert!(
        result.is_err(),
        "verification must reject access outside attenuated scope"
    );
}

#[test]
fn expired_chain_is_rejected() {
    let engine = CapEngine::new();
    let kp_b = BiscuitKeyPair::new();

    let token = engine
        .mint("/**", &[Operation::Read], None, None, None)
        .expect("mint");

    // Expired one hour ago.
    let past = chrono::Utc::now() - Duration::hours(1);
    let token_b = engine
        .attenuate_with_block(&token, &kp_b.private(), &[Caveat::ExpiresAt(past)])
        .expect("B");

    let trust = vec![kp_b.public()];
    let result = engine.verify_multi(&token_b, &trust, "/x", Operation::Read);
    assert!(
        result.is_err(),
        "expired third-party block must reject verification"
    );
}

#[test]
fn empty_trust_set_falls_back_to_authority_only() {
    // Sanity check: no third-party blocks, no trust set — verify_multi
    // should still accept a plain root-minted token.
    let engine = CapEngine::new();
    let token = engine
        .mint("/work/**", &[Operation::Read], None, None, None)
        .expect("mint");

    engine
        .verify_multi(&token, &[], "/work/doc", Operation::Read)
        .expect("plain root token must verify with empty trust set");

    let result = engine.verify_multi(&token, &[], "/home/x", Operation::Read);
    assert!(
        result.is_err(),
        "scope outside root cap must fail even with empty trust"
    );
}
