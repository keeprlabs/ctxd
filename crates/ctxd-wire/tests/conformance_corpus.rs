//! Conformance corpus tests.
//!
//! Pins the daemon's wire encoding and signature verification against
//! the fixtures under `docs/api/conformance/`. Three SDKs (Rust,
//! Python, TS/JS) consume the same corpus: if the daemon's encoder
//! drifts here, every SDK loses faith in the wire contract — these
//! tests catch it first.
//!
//! Layout:
//! - `docs/api/conformance/wire/<name>.json`         logical request/response
//! - `docs/api/conformance/wire/<name>.msgpack.hex`  canonical rmp-serde bytes
//! - `docs/api/conformance/signatures/<name>.json`   event + pubkey + expected
//!
//! See `docs/api/README.md` for how to regenerate the fixtures.

use ctxd_core::event::Event;
use ctxd_core::signing::EventSigner;
use ctxd_wire::messages::{Request, Response};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Workspace-relative path to `docs/api/conformance`. The test
/// binary's `CARGO_MANIFEST_DIR` is `crates/ctxd-wire`; the
/// fixtures live three levels up from that.
fn corpus_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .join("..")
        .join("..")
        .join("docs")
        .join("api")
        .join("conformance")
}

/// Read a file, stripping a single trailing newline if present. The
/// .msgpack.hex files are written with a trailing newline so they're
/// editor-friendly; we don't want that newline polluting the hex
/// comparison.
fn read_trimmed(path: &Path) -> String {
    let mut s =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// Roundtrip a `Request` fixture.
fn assert_request_fixture(name: &str) {
    let dir = corpus_dir().join("wire");
    let json_path = dir.join(format!("{name}.json"));
    let hex_path = dir.join(format!("{name}.msgpack.hex"));

    let json = std::fs::read_to_string(&json_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", json_path.display()));
    let req: Request = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("parse Request from {}: {e}", json_path.display()));
    let bytes = rmp_serde::to_vec(&req)
        .unwrap_or_else(|e| panic!("encode Request from {}: {e}", json_path.display()));
    let actual = encode_hex(&bytes);
    let expected = read_trimmed(&hex_path);

    assert_eq!(
        actual,
        expected,
        "wire/{name}: encoded msgpack hex does not match canonical fixture\n  actual:   {actual}\n  expected: {expected}"
    );
}

/// Roundtrip a `Response` fixture.
fn assert_response_fixture(name: &str) {
    let dir = corpus_dir().join("wire");
    let json_path = dir.join(format!("{name}.json"));
    let hex_path = dir.join(format!("{name}.msgpack.hex"));

    let json = std::fs::read_to_string(&json_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", json_path.display()));
    let resp: Response = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("parse Response from {}: {e}", json_path.display()));
    let bytes = rmp_serde::to_vec(&resp)
        .unwrap_or_else(|e| panic!("encode Response from {}: {e}", json_path.display()));
    let actual = encode_hex(&bytes);
    let expected = read_trimmed(&hex_path);

    assert_eq!(
        actual,
        expected,
        "wire/{name}: encoded msgpack hex does not match canonical fixture\n  actual:   {actual}\n  expected: {expected}"
    );
}

#[test]
fn wire_pub_request_canonical() {
    assert_request_fixture("pub_request");
}

#[test]
fn wire_sub_request_canonical() {
    assert_request_fixture("sub_request");
}

#[test]
fn wire_query_request_canonical() {
    assert_request_fixture("query_request");
}

#[test]
fn wire_grant_request_canonical() {
    assert_request_fixture("grant_request");
}

#[test]
fn wire_revoke_request_canonical() {
    assert_request_fixture("revoke_request");
}

#[test]
fn wire_ping_canonical() {
    assert_request_fixture("ping");
}

#[test]
fn wire_ok_response_canonical() {
    assert_response_fixture("ok_response");
}

#[test]
fn wire_error_response_canonical() {
    assert_response_fixture("error_response");
}

/// Shape of a `signatures/*.json` fixture.
#[derive(Debug, Deserialize)]
struct SignatureFixture {
    /// Free-form description; not asserted against.
    #[allow(dead_code)]
    description: String,
    event: Event,
    signature: String,
    public_key_hex: String,
    expected: bool,
}

fn assert_signature_fixture(name: &str) {
    let path = corpus_dir().join("signatures").join(format!("{name}.json"));
    let json =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let fx: SignatureFixture = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("parse signature fixture {}: {e}", path.display()));

    let pubkey = hex::decode(&fx.public_key_hex)
        .unwrap_or_else(|e| panic!("public_key_hex in {}: {e}", path.display()));
    let actual = EventSigner::verify(&fx.event, &fx.signature, &pubkey);
    assert_eq!(
        actual, fx.expected,
        "signatures/{name}: EventSigner::verify returned {actual}, expected {}",
        fx.expected
    );
}

#[test]
fn signature_valid_fixture() {
    assert_signature_fixture("valid");
}

#[test]
fn signature_tampered_fixture() {
    assert_signature_fixture("tampered");
}

#[test]
fn signature_wrong_key_fixture() {
    assert_signature_fixture("wrong_key");
}
