//! v0.3 hash-chain migration.
//!
//! Pre-v0.3, events did not carry `parents` or `attestation` and therefore
//! their canonical form differed from the v0.3 canonical form. When a
//! pre-v0.3 database is opened against v0.3 code, existing
//! `predecessorhash` values and signatures reference the old canonical
//! form — they're self-consistent within the v0.2 world but won't verify
//! against any v0.3-canonical peer or replica.
//!
//! [`migrate_to_v03`] walks the event log in `seq` order, re-computes
//! each predecessor hash and re-signs each event under v0.3 canonical
//! form, and writes the results back in transactional batches of 1000
//! rows. It is idempotent: running it a second time against an
//! already-migrated database short-circuits unless `--force` is passed.

use ctxd_core::event::Event;
use ctxd_core::hash::PredecessorHash;
use ctxd_core::signing::EventSigner;
use ctxd_core::subject::Subject;
use std::collections::HashMap;

use crate::store::{EventStore, StoreError};

/// The metadata key under which we stamp the schema version once
/// migration succeeds.
pub const CTXD_VERSION_KEY: &str = "ctxd_version";

/// The ctxd schema version this migration targets.
pub const CTXD_VERSION_V03: &str = "0.3";

/// Result of a migration run, useful for humans and for tests.
#[derive(Debug, Clone, Default)]
pub struct MigrationReport {
    /// Number of events the migration considered.
    pub considered: usize,
    /// Number of events whose hash/signature changed.
    pub rewritten: usize,
    /// Number of batch transactions committed.
    pub batches_committed: usize,
    /// Whether the run was a dry run (no writes performed).
    pub dry_run: bool,
    /// Whether the run short-circuited because the database was already migrated.
    pub already_migrated: bool,
}

/// Re-compute predecessor hashes and signatures on every event in the
/// store under v0.3 canonical form.
///
/// ## Batching
///
/// Writes are accumulated in transactions of up to 1000 rows each. If a
/// batch commit fails, earlier batches remain committed but this
/// function returns the error — re-running migration with the default
/// flags (without `--force`) will re-check and resume naturally because
/// rows that already match v0.3 canonical form are a no-op rewrite.
///
/// ## Dry run
///
/// With `dry_run = true`, the function does all the computation but
/// commits no writes. The returned [`MigrationReport`] still accounts
/// for the would-be rewrites.
///
/// ## Idempotency
///
/// After a successful non-dry run, the `ctxd_version` metadata key is
/// set to `"0.3"`. A subsequent call returns immediately with
/// `already_migrated = true` unless `force = true`.
pub async fn migrate_to_v03(
    store: &EventStore,
    signing_key: Option<&[u8]>,
    dry_run: bool,
    force: bool,
) -> Result<MigrationReport, StoreError> {
    if !force {
        if let Some(val) = store.get_metadata(CTXD_VERSION_KEY).await? {
            if val.as_slice() == CTXD_VERSION_V03.as_bytes() {
                return Ok(MigrationReport {
                    already_migrated: true,
                    dry_run,
                    ..Default::default()
                });
            }
        }
    }

    let signer = match signing_key {
        Some(bytes) => Some(
            EventSigner::from_bytes(bytes)
                .map_err(|e| StoreError::Database(sqlx::Error::Protocol(e.to_string())))?,
        ),
        None => None,
    };

    // Fetch all events in log order. For migration we read the entire
    // log up front — the critical invariant is we walk events in `seq`
    // order and maintain a subject -> last-canonical-event map so that
    // each predecessor hash is computed from the v0.3 canonical form
    // of its direct predecessor in the same subject chain.
    let rows: Vec<EventMigrationRow> = sqlx::query_as::<_, EventMigrationRow>(
        r#"
        SELECT seq, id, source, subject, event_type, time, datacontenttype,
               data, predecessorhash, signature, specversion, parents, attestation
        FROM events
        ORDER BY seq ASC
        "#,
    )
    .fetch_all(store.pool())
    .await?;

    let mut report = MigrationReport {
        dry_run,
        considered: rows.len(),
        ..Default::default()
    };

    // Per-subject tail events, used to compute the canonical predecessor
    // hash for the next event in the same subject.
    let mut subject_tails: HashMap<String, Event> = HashMap::new();
    let mut pending: Vec<(i64, Option<String>, Option<String>)> = Vec::new();

    for row in rows {
        let mut event = row.clone().into_event()?;

        // v0.3 canonical predecessor hash is computed from the prior
        // event for the same subject — *after* that prior event has
        // itself been normalized to v0.3 canonical form (which happens
        // implicitly because we walk in seq order and rewrite in place).
        let expected_pred_hash = match subject_tails.get(event.subject.as_str()) {
            Some(prev) => Some(
                PredecessorHash::compute(prev)
                    .map_err(StoreError::Serialization)?
                    .to_string(),
            ),
            None => None,
        };
        event.predecessorhash = expected_pred_hash.clone();

        // Re-sign under v0.3 canonical form if we have a signer and the
        // event was previously signed (or if it ought to be signed —
        // here we only re-sign events that had a signature, to respect
        // the original author's intent).
        let new_signature: Option<String> = match (&signer, row.signature.as_deref()) {
            (Some(s), Some(_)) => Some(
                s.sign(&event)
                    .map_err(|e| StoreError::Database(sqlx::Error::Protocol(e.to_string())))?,
            ),
            // No signer, or event was never signed — leave signature alone.
            _ => row.signature.clone(),
        };
        event.signature = new_signature.clone();

        let changed =
            event.predecessorhash != row.predecessorhash || event.signature != row.signature;
        if changed {
            report.rewritten += 1;
            pending.push((
                row.seq,
                event.predecessorhash.clone(),
                event.signature.clone(),
            ));
        }

        subject_tails.insert(event.subject.as_str().to_string(), event);

        if pending.len() >= 1000 {
            if !dry_run {
                commit_batch(store, &pending).await?;
                report.batches_committed += 1;
            }
            pending.clear();
        }
    }

    if !dry_run && !pending.is_empty() {
        commit_batch(store, &pending).await?;
        report.batches_committed += 1;
    }

    if !dry_run {
        store
            .set_metadata(CTXD_VERSION_KEY, CTXD_VERSION_V03.as_bytes())
            .await?;
    }

    Ok(report)
}

async fn commit_batch(
    store: &EventStore,
    batch: &[(i64, Option<String>, Option<String>)],
) -> Result<(), StoreError> {
    let mut tx = store.pool().begin().await?;
    for (seq, pred, sig) in batch {
        sqlx::query("UPDATE events SET predecessorhash = ?, signature = ? WHERE seq = ?")
            .bind(pred)
            .bind(sig)
            .bind(seq)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct EventMigrationRow {
    seq: i64,
    id: String,
    source: String,
    subject: String,
    event_type: String,
    time: String,
    datacontenttype: String,
    data: String,
    predecessorhash: Option<String>,
    signature: Option<String>,
    specversion: String,
    parents: Option<String>,
    attestation: Option<Vec<u8>>,
}

impl EventMigrationRow {
    fn into_event(self) -> Result<Event, StoreError> {
        let parents: Vec<uuid::Uuid> = match self.parents.as_deref() {
            None | Some("") => Vec::new(),
            Some(s) => s
                .split(',')
                .map(|p| {
                    p.parse::<uuid::Uuid>().map_err(|e| {
                        StoreError::Database(sqlx::Error::Protocol(format!("bad parent uuid: {e}")))
                    })
                })
                .collect::<Result<_, _>>()?,
        };
        Ok(Event {
            specversion: self.specversion,
            id: self.id.parse().map_err(|e| {
                StoreError::Database(sqlx::Error::Protocol(format!("bad uuid: {e}")))
            })?,
            source: self.source,
            subject: Subject::new(&self.subject)?,
            event_type: self.event_type,
            time: chrono::DateTime::parse_from_rfc3339(&self.time)
                .map_err(|e| StoreError::Database(sqlx::Error::Protocol(format!("bad time: {e}"))))?
                .with_timezone(&chrono::Utc),
            datacontenttype: self.datacontenttype,
            data: serde_json::from_str(&self.data)?,
            predecessorhash: self.predecessorhash,
            signature: self.signature,
            parents,
            attestation: self.attestation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use ctxd_core::subject::Subject;

    async fn make_v02_store() -> EventStore {
        // Open a fresh v0.3 store, then manually insert events in a way
        // that mimics a v0.2-canonical database: rows exist in `events`
        // with `parents = NULL` and `attestation = NULL`, but the stored
        // `predecessorhash` was computed against a *v0.2* canonical form
        // (no parents/attestation included). We simulate "v0.2 canonical"
        // by computing a hash over a trimmed form here.
        EventStore::open_memory().await.unwrap()
    }

    /// Compute what a pre-v0.3 predecessor hash would have looked like.
    /// Mirrors the v0.3 canonical form with `parents` and `attestation`
    /// stripped.
    fn v02_style_hash(event: &Event) -> String {
        use sha2::{Digest, Sha256};
        use std::collections::BTreeMap;

        let mut map: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
        map.insert(
            "specversion",
            serde_json::to_value(&event.specversion).unwrap(),
        );
        map.insert("id", serde_json::to_value(event.id).unwrap());
        map.insert("source", serde_json::to_value(&event.source).unwrap());
        map.insert("subject", serde_json::to_value(&event.subject).unwrap());
        map.insert("type", serde_json::to_value(&event.event_type).unwrap());
        map.insert("time", serde_json::to_value(event.time).unwrap());
        map.insert(
            "datacontenttype",
            serde_json::to_value(&event.datacontenttype).unwrap(),
        );
        map.insert("data", event.data.clone());
        let json = serde_json::to_vec(&map).unwrap();
        hex::encode(Sha256::digest(&json))
    }

    #[tokio::test]
    async fn migrate_rewrites_v02_hashes_to_v03_canonical() {
        let store = make_v02_store().await;
        let subject = Subject::new("/mig/a").unwrap();

        // Insert three events in subject /mig/a with v0.2-style predecessor
        // hashes (null on the first, v0.2-canonical on the rest).
        let t0 = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let mut prev: Option<Event> = None;
        for i in 0..3 {
            let mut e = Event::new(
                "ctxd://mig".to_string(),
                subject.clone(),
                "demo".to_string(),
                serde_json::json!({"i": i}),
            );
            e.time = t0 + chrono::Duration::seconds(i);
            let pred = prev.as_ref().map(v02_style_hash);
            insert_raw(&store, &e, pred.as_deref(), None).await;
            prev = Some(e);
        }

        // Dry run: should report 2 rewrites (events 2 and 3 got a predecessor
        // that would have been v0.2-hashed, plus 0 event has a new v0.3 hash
        // since the first event had no predecessor either way).
        let dry = migrate_to_v03(&store, None, true, false).await.unwrap();
        assert!(!dry.already_migrated);
        assert_eq!(dry.considered, 3);
        // Events 2 and 3 need their predecessorhash re-written.
        assert_eq!(dry.rewritten, 2);
        assert_eq!(dry.batches_committed, 0);

        // Real run.
        let real = migrate_to_v03(&store, None, false, false).await.unwrap();
        assert_eq!(real.rewritten, 2);
        assert_eq!(real.batches_committed, 1);

        // Verify: every event's predecessorhash now matches v0.3 canonical
        // form of its predecessor in the same subject.
        let events = store.read(&subject, false).await.unwrap();
        assert_eq!(events.len(), 3);
        assert!(events[0].predecessorhash.is_none());
        for pair in events.windows(2) {
            let expected = PredecessorHash::compute(&pair[0]).unwrap();
            assert_eq!(
                pair[1].predecessorhash.as_deref(),
                Some(expected.as_str()),
                "event {} should have v0.3 predecessorhash matching event {}",
                pair[1].id,
                pair[0].id
            );
        }

        // ctxd_version metadata should now be set.
        let v = store.get_metadata(CTXD_VERSION_KEY).await.unwrap().unwrap();
        assert_eq!(&v, CTXD_VERSION_V03.as_bytes());

        // Re-running is a no-op.
        let noop = migrate_to_v03(&store, None, false, false).await.unwrap();
        assert!(noop.already_migrated);
        assert_eq!(noop.rewritten, 0);
    }

    #[tokio::test]
    async fn migrate_re_signs_events_when_key_provided() {
        let store = EventStore::open_memory().await.unwrap();
        let subject = Subject::new("/mig/signed").unwrap();

        let signer = EventSigner::new();
        let pk = signer.public_key_bytes();

        // Insert an event with a v0.2-style signature (signed over v0.2
        // canonical form).
        let mut e = Event::new(
            "ctxd://mig".to_string(),
            subject.clone(),
            "demo".to_string(),
            serde_json::json!({"hello": "world"}),
        );
        e.time = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        // Forge a "v0.2 signature" by signing a tweaked clone without
        // parents/attestation. We don't have code to do that directly,
        // so simulate by using a garbage signature string — the test
        // then asserts that after migration the signature verifies.
        let fake_sig = "aa".repeat(64);
        insert_raw(&store, &e, None, Some(&fake_sig)).await;

        // Migration with signer should re-sign in v0.3 canonical form.
        let rep = migrate_to_v03(&store, Some(&signer.secret_key_bytes()), false, false)
            .await
            .unwrap();
        assert_eq!(rep.rewritten, 1);

        let events = store.read(&subject, false).await.unwrap();
        let sig_b64 = events[0].signature.as_ref().expect("signature present");
        assert!(EventSigner::verify(&events[0], sig_b64, &pk));
    }

    /// Helper: insert an event row directly, bypassing `append` so we
    /// can control the `predecessorhash` value to simulate a v0.2 store.
    async fn insert_raw(
        store: &EventStore,
        event: &Event,
        predecessorhash: Option<&str>,
        signature: Option<&str>,
    ) {
        sqlx::query(
            r#"
            INSERT INTO events (id, source, subject, event_type, time, datacontenttype,
                                data, predecessorhash, signature, specversion, parents, attestation)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL)
            "#,
        )
        .bind(event.id.to_string())
        .bind(&event.source)
        .bind(event.subject.as_str())
        .bind(&event.event_type)
        .bind(event.time.to_rfc3339())
        .bind(&event.datacontenttype)
        .bind(serde_json::to_string(&event.data).unwrap())
        .bind(predecessorhash)
        .bind(signature)
        .bind(&event.specversion)
        .execute(store.pool())
        .await
        .unwrap();
    }
}
