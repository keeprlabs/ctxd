//! Canonical msgpack emitter for the wire conformance corpus.
//!
//! This test prints, for every wire fixture, the logical JSON
//! representation of a `Request`/`Response` and the canonical
//! `rmp-serde`-encoded bytes hex-dumped on a single line. Run it
//! when authoring or refreshing `docs/api/conformance/wire/*.msgpack.hex`:
//!
//! ```sh
//! cargo test -p ctxd-wire --test conformance_emit -- --ignored --nocapture
//! ```
//!
//! It is `#[ignore]`d so it doesn't run on every `cargo test` —
//! the assertion that the daemon's encoder still matches the
//! committed bytes lives in `conformance_corpus.rs`.

use ctxd_core::event::Event;
use ctxd_core::signing::EventSigner;
use ctxd_core::subject::Subject;
use ctxd_wire::messages::{Request, Response};
use serde_json::json;

fn hexdump(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

fn emit_request(name: &str, req: &Request) {
    let bytes = rmp_serde::to_vec(req).expect("encode request");
    let logical = serde_json::to_string_pretty(req).expect("logical json");
    println!("=== wire/{name}_request ===");
    println!("logical:\n{logical}");
    println!("hex:\n{}", hexdump(&bytes));
    println!();
}

fn emit_response(name: &str, resp: &Response) {
    let bytes = rmp_serde::to_vec(resp).expect("encode response");
    let logical = serde_json::to_string_pretty(resp).expect("logical json");
    println!("=== wire/{name} ===");
    println!("logical:\n{logical}");
    println!("hex:\n{}", hexdump(&bytes));
    println!();
}

#[test]
#[ignore]
fn emit_canonical_corpus() {
    emit_request(
        "pub",
        &Request::Pub {
            subject: "/test/hello".to_string(),
            event_type: "demo".to_string(),
            data: json!({ "msg": "world" }),
        },
    );

    emit_request(
        "sub",
        &Request::Sub {
            subject_pattern: "/work/**".to_string(),
        },
    );

    emit_request(
        "query",
        &Request::Query {
            subject_pattern: "/work/**".to_string(),
            view: "log".to_string(),
        },
    );

    emit_request(
        "grant",
        &Request::Grant {
            subject: "/work/**".to_string(),
            operations: vec!["read".to_string(), "write".to_string()],
            expiry: None,
        },
    );

    emit_request(
        "revoke",
        &Request::Revoke {
            cap_id: "cap-01".to_string(),
        },
    );

    // Ping has no fields — encoded as the bare string "Ping".
    let bytes = rmp_serde::to_vec(&Request::Ping).expect("encode ping");
    println!("=== wire/ping ===");
    println!("logical:\n\"Ping\"");
    println!("hex:\n{}", hexdump(&bytes));
    println!();

    emit_response(
        "ok_response",
        &Response::Ok {
            data: json!({
                "id": "01900000-0000-7000-8000-000000000001",
                "predecessorhash": null,
            }),
        },
    );

    emit_response(
        "error_response",
        &Response::Error {
            message: "permission denied".to_string(),
        },
    );

    // Signed event fixture: deterministic key + event so the
    // signature can be checked into the corpus and re-verified.
    let secret_bytes = [7u8; 32];
    let signer = EventSigner::from_bytes(&secret_bytes).expect("from_bytes");
    let pubkey = signer.public_key_bytes();

    let mut signed = Event {
        specversion: "1.0".to_string(),
        id: uuid::Uuid::parse_str("01900000-0000-7000-8000-000000000030").unwrap(),
        source: "ctxd://conformance".to_string(),
        subject: Subject::new("/conformance/signed").unwrap(),
        event_type: "demo".to_string(),
        time: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:03:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        datacontenttype: "application/json".to_string(),
        data: json!({ "msg": "hello" }),
        predecessorhash: None,
        signature: None,
        parents: Vec::new(),
        attestation: None,
    };
    let sig = signer.sign(&signed).expect("sign");
    signed.signature = Some(sig.clone());

    println!("=== events/signed ===");
    println!(
        "logical:\n{}",
        serde_json::to_string_pretty(&signed).expect("ev json")
    );
    println!("pubkey_hex:\n{}", hexdump(&pubkey));
    println!("signature_hex:\n{}", sig);
    println!();

    // Wrong-key fixture — a *different* deterministic pubkey, used to
    // assert that verification with a non-matching pubkey returns
    // false.
    let other = EventSigner::from_bytes(&[9u8; 32]).expect("other from_bytes");
    println!("=== signatures/wrong_key ===");
    println!("wrong_pubkey_hex:\n{}", hexdump(&other.public_key_bytes()));
    println!();
}
