//! Arrow + Parquet I/O for event parts.
//!
//! The Parquet schema mirrors the SQLite `events` table so a manifest
//! full of Parquet parts is analytically queryable via DuckDB's
//! `read_parquet()` with no additional schema plumbing. Columns:
//!
//! - `seq` — INT64 primary ordering key.
//! - `id` — FIXED_LEN_BYTE_ARRAY(16) — raw UUID bytes.
//! - `source` — TEXT.
//! - `subject` — TEXT.
//! - `event_type` — TEXT.
//! - `time` — TIMESTAMP(nanoseconds, UTC).
//! - `datacontenttype` — TEXT.
//! - `data` — BYTES (UTF-8 JSON).
//! - `predecessorhash` — TEXT, nullable.
//! - `signature` — TEXT, nullable.
//! - `parents` — LIST<FIXED_LEN_BYTE_ARRAY(16)> — sorted UUID bytes.
//! - `attestation` — BYTES, nullable.
//! - `specversion` — TEXT.
//!
//! We compress with Snappy (fast decode, decent ratio) and write a
//! single row group per part — parts are small enough (≤64 MiB) that
//! extra row groups add complexity without winning prune time.

use std::collections::HashSet;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BinaryArray, BinaryBuilder, FixedSizeBinaryArray, FixedSizeBinaryBuilder,
    Int64Array, ListBuilder, RecordBatch, StringArray, StringBuilder, TimestampNanosecondArray,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use bytes::Bytes;
use chrono::TimeZone;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use object_store::path::Path as ObjPath;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use uuid::Uuid;

use crate::manifest::Manifest;
use crate::store::StoreError;

/// Build the Arrow schema for a Parquet event part. Stable — the
/// column layout is forward-compatible only via additive migrations.
pub fn event_schema() -> Arc<Schema> {
    // ListBuilder's default inner field is nullable; keep the schema aligned.
    let parent_item = Field::new("item", DataType::FixedSizeBinary(16), true);
    Arc::new(Schema::new(vec![
        Field::new("seq", DataType::Int64, false),
        Field::new("id", DataType::FixedSizeBinary(16), false),
        Field::new("source", DataType::Utf8, false),
        Field::new("subject", DataType::Utf8, false),
        Field::new("event_type", DataType::Utf8, false),
        Field::new(
            "time",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            false,
        ),
        Field::new("datacontenttype", DataType::Utf8, false),
        Field::new("data", DataType::Binary, false),
        Field::new("predecessorhash", DataType::Utf8, true),
        Field::new("signature", DataType::Utf8, true),
        Field::new("parents", DataType::List(Arc::new(parent_item)), false),
        Field::new("attestation", DataType::Binary, true),
        Field::new("specversion", DataType::Utf8, false),
    ]))
}

/// Build a [`RecordBatch`] from `(seq, event)` pairs.
pub fn build_batch(rows: &[(i64, Event)]) -> Result<RecordBatch, StoreError> {
    let schema = event_schema();
    let mut seq_vals: Vec<i64> = Vec::with_capacity(rows.len());
    let mut id_builder = FixedSizeBinaryBuilder::new(16);
    let mut source = StringBuilder::new();
    let mut subject = StringBuilder::new();
    let mut event_type = StringBuilder::new();
    let mut time_vals: Vec<i64> = Vec::with_capacity(rows.len());
    let mut datacontenttype = StringBuilder::new();
    let mut data = BinaryBuilder::new();
    let mut predecessorhash = StringBuilder::new();
    let mut signature = StringBuilder::new();
    let mut parents_builder: ListBuilder<FixedSizeBinaryBuilder> =
        ListBuilder::new(FixedSizeBinaryBuilder::new(16));
    let mut attestation = BinaryBuilder::new();
    let mut specversion = StringBuilder::new();

    for (seq, event) in rows {
        seq_vals.push(*seq);
        id_builder
            .append_value(event.id.as_bytes())
            .map_err(StoreError::Arrow)?;
        source.append_value(&event.source);
        subject.append_value(event.subject.as_str());
        event_type.append_value(&event.event_type);
        // Timestamps are UTC nanoseconds. chrono's `timestamp_nanos_opt`
        // is fallible on far-future timestamps; fall back to micros * 1000.
        let ts_ns = event
            .time
            .timestamp_nanos_opt()
            .unwrap_or_else(|| event.time.timestamp_micros() * 1_000);
        time_vals.push(ts_ns);
        datacontenttype.append_value(&event.datacontenttype);
        let data_bytes = serde_json::to_vec(&event.data)?;
        data.append_value(&data_bytes);
        match &event.predecessorhash {
            Some(s) => predecessorhash.append_value(s),
            None => predecessorhash.append_null(),
        }
        match &event.signature {
            Some(s) => signature.append_value(s),
            None => signature.append_null(),
        }
        // Canonical sorted parents — idempotent since append() already
        // sorts, but we sort defensively here so Parquet-only callers
        // see the invariant too.
        let mut parents = event.parents.clone();
        parents.sort();
        for p in &parents {
            parents_builder
                .values()
                .append_value(p.as_bytes())
                .map_err(StoreError::Arrow)?;
        }
        parents_builder.append(true);
        match &event.attestation {
            Some(b) => attestation.append_value(b),
            None => attestation.append_null(),
        }
        specversion.append_value(&event.specversion);
    }

    let seq_array = Arc::new(Int64Array::from(seq_vals)) as ArrayRef;
    let id_array = Arc::new(id_builder.finish()) as ArrayRef;
    let source_array = Arc::new(source.finish()) as ArrayRef;
    let subject_array = Arc::new(subject.finish()) as ArrayRef;
    let event_type_array = Arc::new(event_type.finish()) as ArrayRef;
    let time_array =
        Arc::new(TimestampNanosecondArray::from(time_vals).with_timezone("UTC")) as ArrayRef;
    let datacontenttype_array = Arc::new(datacontenttype.finish()) as ArrayRef;
    let data_array = Arc::new(data.finish()) as ArrayRef;
    let predecessor_array = Arc::new(predecessorhash.finish()) as ArrayRef;
    let signature_array = Arc::new(signature.finish()) as ArrayRef;
    let parents_array = Arc::new(parents_builder.finish()) as ArrayRef;
    let attestation_array = Arc::new(attestation.finish()) as ArrayRef;
    let specversion_array = Arc::new(specversion.finish()) as ArrayRef;

    RecordBatch::try_new(
        schema,
        vec![
            seq_array,
            id_array,
            source_array,
            subject_array,
            event_type_array,
            time_array,
            datacontenttype_array,
            data_array,
            predecessor_array,
            signature_array,
            parents_array,
            attestation_array,
            specversion_array,
        ],
    )
    .map_err(StoreError::Arrow)
}

/// Encode a batch to Parquet bytes (Snappy-compressed, single row group).
pub fn encode_parquet(batch: &RecordBatch) -> Result<Vec<u8>, StoreError> {
    let mut buf: Vec<u8> = Vec::new();
    let props = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build();
    let mut writer =
        ArrowWriter::try_new(&mut buf, batch.schema(), Some(props)).map_err(StoreError::Parquet)?;
    writer.write(batch).map_err(StoreError::Parquet)?;
    writer.close().map_err(StoreError::Parquet)?;
    Ok(buf)
}

/// Decode Parquet bytes back into events. Preserves row order.
pub fn decode_parquet(bytes: Bytes) -> Result<Vec<Event>, StoreError> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes)
        .map_err(StoreError::Parquet)?
        .build()
        .map_err(StoreError::Parquet)?;

    let mut out: Vec<Event> = Vec::new();
    for batch in reader {
        let batch = batch.map_err(StoreError::Arrow)?;
        out.extend(batch_to_events(&batch)?);
    }
    Ok(out)
}

fn batch_to_events(batch: &RecordBatch) -> Result<Vec<Event>, StoreError> {
    let num_rows = batch.num_rows();
    let schema = batch.schema();

    let col = |name: &str| -> Result<usize, StoreError> {
        schema
            .index_of(name)
            .map_err(|e| StoreError::Decode(format!("missing column {name}: {e}")))
    };

    let id_idx = col("id")?;
    let source_idx = col("source")?;
    let subject_idx = col("subject")?;
    let event_type_idx = col("event_type")?;
    let time_idx = col("time")?;
    let datacontenttype_idx = col("datacontenttype")?;
    let data_idx = col("data")?;
    let predecessor_idx = col("predecessorhash")?;
    let signature_idx = col("signature")?;
    let parents_idx = col("parents")?;
    let attestation_idx = col("attestation")?;
    let specversion_idx = col("specversion")?;

    let id_array = batch
        .column(id_idx)
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .ok_or_else(|| StoreError::Decode("id column not FixedSizeBinary(16)".to_string()))?;
    let source_array = batch
        .column(source_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| StoreError::Decode("source column not Utf8".to_string()))?;
    let subject_array = batch
        .column(subject_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| StoreError::Decode("subject column not Utf8".to_string()))?;
    let event_type_array = batch
        .column(event_type_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| StoreError::Decode("event_type column not Utf8".to_string()))?;
    let time_array = batch
        .column(time_idx)
        .as_any()
        .downcast_ref::<TimestampNanosecondArray>()
        .ok_or_else(|| StoreError::Decode("time column not TimestampNanosecond".to_string()))?;
    let datacontenttype_array = batch
        .column(datacontenttype_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| StoreError::Decode("datacontenttype column not Utf8".to_string()))?;
    let data_array = batch
        .column(data_idx)
        .as_any()
        .downcast_ref::<BinaryArray>()
        .ok_or_else(|| StoreError::Decode("data column not Binary".to_string()))?;
    let predecessor_array = batch
        .column(predecessor_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| StoreError::Decode("predecessorhash column not Utf8".to_string()))?;
    let signature_array = batch
        .column(signature_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| StoreError::Decode("signature column not Utf8".to_string()))?;
    let parents_array = batch
        .column(parents_idx)
        .as_any()
        .downcast_ref::<arrow::array::ListArray>()
        .ok_or_else(|| StoreError::Decode("parents column not List".to_string()))?;
    let attestation_array = batch
        .column(attestation_idx)
        .as_any()
        .downcast_ref::<BinaryArray>()
        .ok_or_else(|| StoreError::Decode("attestation column not Binary".to_string()))?;
    let specversion_array = batch
        .column(specversion_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| StoreError::Decode("specversion column not Utf8".to_string()))?;

    let mut out = Vec::with_capacity(num_rows);
    for row in 0..num_rows {
        let id_bytes = id_array.value(row);
        let id =
            Uuid::from_slice(id_bytes).map_err(|e| StoreError::Decode(format!("bad uuid: {e}")))?;
        let source = source_array.value(row).to_string();
        let subject = subject_array.value(row).to_string();
        let event_type = event_type_array.value(row).to_string();
        let time_ns = time_array.value(row);
        let time = chrono::Utc.timestamp_nanos(time_ns);
        let datacontenttype = datacontenttype_array.value(row).to_string();
        let data_bytes = data_array.value(row);
        let data: serde_json::Value = serde_json::from_slice(data_bytes)?;
        let predecessorhash = if predecessor_array.is_null(row) {
            None
        } else {
            Some(predecessor_array.value(row).to_string())
        };
        let signature = if signature_array.is_null(row) {
            None
        } else {
            Some(signature_array.value(row).to_string())
        };
        let parents_list = parents_array.value(row);
        let parents_fb = parents_list
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .ok_or_else(|| {
                StoreError::Decode("parents inner array not FixedSizeBinary(16)".to_string())
            })?;
        let mut parents = Vec::with_capacity(parents_fb.len());
        for i in 0..parents_fb.len() {
            let pb = parents_fb.value(i);
            parents.push(
                Uuid::from_slice(pb).map_err(|e| StoreError::Decode(format!("parent: {e}")))?,
            );
        }
        let attestation = if attestation_array.is_null(row) {
            None
        } else {
            Some(attestation_array.value(row).to_vec())
        };
        let specversion = specversion_array.value(row).to_string();

        out.push(Event {
            specversion,
            id,
            source,
            subject: Subject::new(&subject)?,
            event_type,
            time,
            datacontenttype,
            data,
            predecessorhash,
            signature,
            parents,
            attestation,
        });
    }
    Ok(out)
}

/// PUT a new Parquet part under `path` with the given rows.
#[tracing::instrument(skip(store, rows))]
pub async fn write_part(
    store: &dyn ObjectStore,
    path: &ObjPath,
    rows: &[(i64, Event)],
) -> Result<(), StoreError> {
    let batch = build_batch(rows)?;
    let bytes = encode_parquet(&batch)?;
    store
        .put(path, PutPayload::from(Bytes::from(bytes)))
        .await?;
    Ok(())
}

/// GET a Parquet part and decode it into events.
#[tracing::instrument(skip(store))]
pub async fn read_part(store: &dyn ObjectStore, path: &ObjPath) -> Result<Vec<Event>, StoreError> {
    let res = store.get(path).await?;
    let bytes = res.bytes().await?;
    decode_parquet(bytes)
}

/// Collect all event ids across sealed parts. Used on cold-start WAL
/// replay to dedup.
pub async fn collect_sealed_event_ids(
    store: &dyn ObjectStore,
    manifest: &Manifest,
) -> Result<HashSet<Uuid>, StoreError> {
    let mut out = HashSet::new();
    for p in &manifest.parts {
        let obj = ObjPath::parse(&p.path)?;
        let events = read_part(store, &obj).await?;
        for e in events {
            out.insert(e.id);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_batch_encodes_and_decodes() {
        let subject = Subject::new("/a/b").unwrap();
        let mut e = Event::new(
            "src".into(),
            subject.clone(),
            "t".into(),
            serde_json::json!({"v": 1, "s": "hi"}),
        );
        let p1 = Uuid::parse_str("00000000-0000-7000-8000-000000000001").unwrap();
        let p2 = Uuid::parse_str("00000000-0000-7000-8000-000000000002").unwrap();
        e.parents = vec![p2, p1];
        e.attestation = Some(vec![0xde, 0xad, 0xbe, 0xef]);
        e.predecessorhash = Some("abc".into());
        e.signature = Some("sig".into());

        let batch = build_batch(&[(7, e.clone())]).unwrap();
        let bytes = encode_parquet(&batch).unwrap();
        let decoded = decode_parquet(Bytes::from(bytes)).unwrap();
        assert_eq!(decoded.len(), 1);
        let d = &decoded[0];
        assert_eq!(d.id, e.id);
        assert_eq!(d.subject.as_str(), "/a/b");
        assert_eq!(d.parents, vec![p1, p2]);
        assert_eq!(
            d.attestation.as_deref(),
            Some(&[0xde, 0xad, 0xbe, 0xefu8][..])
        );
        assert_eq!(d.predecessorhash.as_deref(), Some("abc"));
        assert_eq!(d.signature.as_deref(), Some("sig"));
    }
}
