//! Parquet must preserve `parents` (UUID list) and `attestation`
//! (binary) byte-identically — federation depends on this.

use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_duckobj::DuckObjStore;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn parents_and_attestation_roundtrip_through_parquet() {
    let td = TempDir::new().unwrap();
    let root = td.path().to_path_buf();
    let store = DuckObjStore::open_local(&root).await.unwrap();

    let subject = Subject::new("/merge/duck").unwrap();
    let p1 = Uuid::parse_str("00000000-0000-7000-8000-0000000000a1").unwrap();
    let p2 = Uuid::parse_str("00000000-0000-7000-8000-0000000000a2").unwrap();
    let p3 = Uuid::parse_str("00000000-0000-7000-8000-0000000000a3").unwrap();

    let attest = vec![0xca, 0xfe, 0xba, 0xbe, 0x01, 0x02, 0x03];

    let mut e = Event::new(
        "ctxd://test".to_string(),
        subject.clone(),
        "demo".to_string(),
        serde_json::json!({"merge": true, "rationale": "cross-peer reconciliation"}),
    );
    e.parents = vec![p3, p1, p2]; // intentionally unsorted
    e.attestation = Some(attest.clone());

    let stored = store.append(e).await.unwrap();
    assert_eq!(stored.parents, vec![p1, p2, p3]);
    // Force a flush so the round-trip actually goes through Parquet
    // and not just the in-memory buffer.
    store.flush().await.unwrap();

    let events = store.read(&subject, false).await.unwrap();
    assert_eq!(events.len(), 1);
    let back = &events[0];
    assert_eq!(back.id, stored.id);
    assert_eq!(back.parents, vec![p1, p2, p3]);
    assert_eq!(back.attestation.as_deref(), Some(attest.as_slice()));
    assert_eq!(
        back.data,
        serde_json::json!({"merge": true, "rationale": "cross-peer reconciliation"})
    );
}
