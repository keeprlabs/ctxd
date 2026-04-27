//! Conformance tests against the canonical `docs/api/conformance/`
//! corpus.
//!
//! These tests prove the SDK side of the wire / signature / event
//! contract independently of the daemon's own conformance suite.
//! Both sides share the same fixture files, so any drift between
//! daemon and SDK breaks here first.

use std::fs;
use std::path::{Path, PathBuf};

use ctxd_client::{verify_signature, Event};

/// Locate the workspace root (dir containing `Cargo.lock`) starting
/// from the SDK crate's manifest dir.
fn workspace_root() -> PathBuf {
    let mut cursor = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if cursor.join("Cargo.lock").exists() {
            return cursor;
        }
        match cursor.parent() {
            Some(p) => cursor = p.to_path_buf(),
            None => panic!("workspace root not found from {:?}", env!("CARGO_MANIFEST_DIR")),
        }
    }
}

fn corpus_dir(category: &str) -> PathBuf {
    workspace_root()
        .join("docs")
        .join("api")
        .join("conformance")
        .join(category)
}

#[test]
fn signatures_corpus_matches_expected() {
    let dir = corpus_dir("signatures");
    let mut count = 0;
    for entry in fs::read_dir(&dir).expect("read signatures dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read fixture");
        let json: serde_json::Value = serde_json::from_str(&text).expect("parse fixture");
        // Each fixture has shape:
        //   { event: {...}, signature: "...", public_key_hex: "...", expected: bool }
        // The signature lives in the side-channel field, not on the
        // event itself, so we splice it onto a deserialized Event
        // before calling verify_signature (which expects to read
        // event.signature).
        let mut event: Event = serde_json::from_value(json["event"].clone())
            .unwrap_or_else(|e| panic!("deserialize event in {path:?}: {e}"));
        let sig = json["signature"]
            .as_str()
            .unwrap_or_else(|| panic!("missing `signature` in {path:?}"))
            .to_string();
        let pubkey_hex = json["public_key_hex"]
            .as_str()
            .unwrap_or_else(|| panic!("missing `public_key_hex` in {path:?}"))
            .to_string();
        let expected = json["expected"]
            .as_bool()
            .unwrap_or_else(|| panic!("missing `expected` in {path:?}"));

        event.signature = Some(sig);
        let actual = verify_signature(&event, &pubkey_hex)
            .unwrap_or_else(|e| panic!("verify_signature errored on {path:?}: {e}"));

        assert_eq!(
            actual,
            expected,
            "signature fixture {} expected {} got {}",
            path.display(),
            expected,
            actual
        );
        count += 1;
    }
    assert!(count >= 3, "expected at least 3 signature fixtures, found {count}");
}

#[test]
fn wire_corpus_msgpack_hex_roundtrips() {
    use ctxd_client::wire::{WireRequest, WireResponse};
    let dir = corpus_dir("wire");
    // Map: stem → (json_path, hex_path).
    let mut pairs: std::collections::BTreeMap<String, (Option<PathBuf>, Option<PathBuf>)> =
        std::collections::BTreeMap::new();
    for entry in fs::read_dir(&dir).expect("read wire dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if let Some(stem) = name.strip_suffix(".msgpack.hex") {
            pairs.entry(stem.to_string()).or_default().1 = Some(path);
        } else if let Some(stem) = name.strip_suffix(".json") {
            pairs.entry(stem.to_string()).or_default().0 = Some(path);
        }
    }
    assert!(!pairs.is_empty(), "no wire fixtures found in {dir:?}");

    let mut covered = 0;
    for (stem, (json_path, hex_path)) in pairs {
        let json_path = json_path.unwrap_or_else(|| panic!("missing JSON fixture for {stem}"));
        let hex_path =
            hex_path.unwrap_or_else(|| panic!("missing msgpack.hex fixture for {stem}"));

        let json_text = fs::read_to_string(&json_path).expect("read json");
        let hex_text = fs::read_to_string(&hex_path).expect("read hex");
        let expected_bytes = hex::decode(hex_text.trim()).expect("decode hex");

        // Decide which type to deserialize as based on the file name.
        // Fixtures that end in `_response` are Responses; everything
        // else is a Request.
        let actual_bytes: Vec<u8> = if stem.ends_with("_response") {
            let value: serde_json::Value =
                serde_json::from_str(&json_text).expect("parse response json");
            let resp: WireResponse =
                serde_json::from_value(value).expect("deserialize response");
            rmp_serde::to_vec(&resp).expect("encode msgpack")
        } else {
            let value: serde_json::Value =
                serde_json::from_str(&json_text).expect("parse request json");
            let req: WireRequest =
                serde_json::from_value(value).expect("deserialize request");
            rmp_serde::to_vec(&req).expect("encode msgpack")
        };

        assert_eq!(
            actual_bytes,
            expected_bytes,
            "wire fixture `{stem}` msgpack mismatch:\n  expected: {hex_text}\n  actual:   {}",
            hex::encode(&actual_bytes)
        );
        covered += 1;
    }
    assert!(covered >= 5, "expected >= 5 wire fixtures, got {covered}");
}

#[test]
fn events_corpus_roundtrips_structurally() {
    let dir = corpus_dir("events");
    let mut count = 0;
    for entry in fs::read_dir(&dir).expect("read events dir") {
        let entry = entry.expect("entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read");

        // Parse + re-serialize via Event. The structural assertion
        // is: the *parsed Value* of input and re-serialized output
        // are equal once the optional fields converge. We use
        // sorted-keys structural equality (serde_json::Value's PartialEq).
        let original: serde_json::Value = serde_json::from_str(&text).expect("parse json");
        let event: Event = serde_json::from_value(original.clone())
            .unwrap_or_else(|e| panic!("deserialize event {path:?}: {e}"));
        let reserialized = serde_json::to_value(&event)
            .unwrap_or_else(|e| panic!("re-serialize event {path:?}: {e}"));

        assert_eq!(
            original,
            reserialized,
            "event fixture {} did not roundtrip:\n  original: {}\n  reserialized: {}",
            path.display(),
            serde_json::to_string_pretty(&original).unwrap_or_default(),
            serde_json::to_string_pretty(&reserialized).unwrap_or_default(),
        );
        count += 1;
    }
    assert!(count >= 3, "expected at least 3 event fixtures, found {count}");

    // Smoke-check the signed fixture in particular — the SDK can
    // verify it against the side-channel pubkey hex.
    let signed_path = dir.join("signed.json");
    let pubkey_path = dir.join("signed.pubkey.hex");
    let signed_text = fs::read_to_string(&signed_path).expect("read signed.json");
    let pubkey_hex = fs::read_to_string(&pubkey_path).expect("read pubkey hex");
    let event: Event = serde_json::from_str(&signed_text).expect("deserialize signed");
    let ok = verify_signature(&event, pubkey_hex.trim()).expect("verify");
    assert!(ok, "signed event in events/signed.json must verify");
}

/// Smoke-check that workspace_root() actually points to a directory
/// with the conformance corpus — guards against test-runner
/// reconfigurations silently skipping the assertions above.
#[test]
fn corpus_root_exists() {
    let root = workspace_root();
    let conformance = root.join("docs/api/conformance");
    assert!(
        conformance.exists() && Path::is_dir(&conformance),
        "docs/api/conformance must exist at {}",
        conformance.display()
    );
}
