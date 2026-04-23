# Benchmarking ctxd

How to measure ctxd's performance and compare it to alternatives.

## ctxd benchmark results

Run with `cargo bench -p ctxd-store` on an in-memory SQLite store. Full details in [benchmark-results.md](benchmark-results.md).

| Operation | Latency | Notes |
|-----------|---------|-------|
| Append single event | 2.85 ms | Includes store initialization |
| Append 100 sequential | 375 us/event | Amortized over batch |
| Read exact (1 event) | 79.74 us | Single subject, single event |
| Read recursive (100 events) | 1.09 ms | All events under one prefix |
| Read recursive (1000 events) | 10.03 ms | All events under one prefix |
| FTS search (100 events) | 987.17 us | SQLite FTS5 |
| FTS search (10,000 events) | 105.87 ms | SQLite FTS5 |
| KV get latest | 68.92 us | Latest value for a subject |

Environment: release build, in-memory SQLite, Criterion v0.5, 100 samples.

## Comparison with alternatives

ctxd is not a database, a message broker, or a vector store. It is a context substrate that combines features from all three. Comparisons below are operation-specific -- each system is benchmarked on the operations it was designed for.

### Feature comparison

| | ctxd | Redis | NATS | SQLite (raw) | Mem0 | ChromaDB |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| Open source | Apache-2.0 | BSD-3 | Apache-2.0 | Public domain | Partial | Apache-2.0 |
| Self-hosted single binary | Yes | Yes | Yes | Library | No (cloud) | Yes |
| MCP native | Yes | No | No | No | Partial | No |
| Append-only event log | Yes | No | Yes (JetStream) | No | No | No |
| Tamper-evident hash chains | Yes | No | No | No | No | No |
| Capability-based auth | Yes (Biscuit) | ACL | NKEY/JWT | None | API key | None |
| Full-text search | Yes (FTS5) | RediSearch | No | Manual | Via LLM | No |
| Vector search | Yes (HNSW) | RedisVSS | No | No | Yes | Yes |
| Subject-path addressing | Yes | Key-based | Subject-based | Tables | User/session | Collections |

### Write throughput

| System | Operation | Throughput | Source |
|--------|-----------|------------|--------|
| **ctxd** | append (in-process) | ~2,667 events/sec | Criterion bench, sequential appends |
| **Redis** | SET (pipelined) | ~100,000+ ops/sec | redis-benchmark, single node |
| **NATS** | PUB (no persistence) | ~10M+ msgs/sec | nats bench, single node |
| **NATS JetStream** | PUB (persisted) | ~200,000 msgs/sec | NATS docs, single node, file store |
| **SQLite** | INSERT (WAL mode) | ~50,000 rows/sec | Published benchmarks, single writer |
| **Mem0** | add() | ~5-50 ops/sec | Requires LLM call per write |
| **ChromaDB** | add (batch 1000) | ~1,000-5,000 docs/sec | Published benchmarks, depends on embedding model |

**Why ctxd is slower than raw Redis/NATS/SQLite:** Every ctxd append does more work per operation:
1. Fetch the previous event for predecessor hash computation (1 read)
2. Compute SHA-256 hash of the canonical form
3. Generate UUIDv7
4. INSERT into the event log
5. UPSERT into the KV view
6. INSERT into the FTS index
7. All within a single SQLite transaction

This is 3 writes + 1 read + 1 hash per append, compared to Redis's single key-value SET or NATS's single message publish.

### Read latency

| System | Operation | Latency | Source |
|--------|-----------|---------|--------|
| **ctxd** | exact read (1 event) | 80 us | Criterion bench |
| **ctxd** | recursive read (100 events) | 1.09 ms | Criterion bench |
| **Redis** | GET | ~0.1-0.5 ms | redis-benchmark, single node |
| **NATS JetStream** | fetch from stream | ~0.5-2 ms | NATS docs |
| **SQLite** | SELECT by indexed key | ~10-50 us | Published benchmarks |
| **Mem0** | get_all() | ~50-200 ms | Network + DB lookup |

### Search latency

| System | Operation | Latency | Source |
|--------|-----------|---------|--------|
| **ctxd** | FTS (100 events) | 987 us | Criterion bench |
| **ctxd** | FTS (10k events) | 105.87 ms | Criterion bench |
| **Redis + RediSearch** | FT.SEARCH (100k docs) | ~1-5 ms | Redis docs |
| **ChromaDB** | query (10k vectors, k=10) | ~5-20 ms | Published benchmarks |
| **Mem0** | search() | ~100-500 ms | Requires LLM for semantic search |

### Vector search

| System | Operation | Latency | Source |
|--------|-----------|---------|--------|
| **ctxd** | k-NN (HNSW, in-memory) | Not yet benchmarked | instant-distance crate |
| **ChromaDB** | query (10k vectors, k=10) | ~5-20 ms | Published benchmarks |
| **Qdrant** | search (1M vectors, k=10) | ~5-15 ms | Qdrant benchmarks |
| **Redis + RedisVSS** | FT.SEARCH (100k vectors) | ~1-10 ms | Redis docs |

## Where ctxd wins

- **All-in-one for AI agents.** One binary gives you an event log, KV store, FTS index, vector search, capability auth, and MCP tools. No Redis + Elasticsearch + Qdrant + custom auth stack.
- **Tamper evidence.** No other system in this category provides hash-chain integrity out of the box. If an event is modified after the fact, the chain breaks.
- **Capability-based auth with attenuation.** Give a sub-agent a scoped, time-limited, narrowed token. It cannot escalate. Redis ACLs and NATS NKEYs do not support delegation chains.
- **MCP native.** Connect Claude Desktop or Cursor in 30 seconds. No adapter layer, no SDK, no glue code.
- **Zero external dependencies.** No Docker Compose, no managed service, no API keys. `cargo build && ctxd serve`.
- **Subject-path addressing.** Hierarchical namespace with recursive reads and glob-based capability scoping. Natural fit for how context is organized (by project, team, entity).

## Where ctxd loses

- **Raw throughput.** Redis and NATS are purpose-built for speed. ctxd does more work per operation (hash chains, view updates, capability checks). If you need 100k+ writes/sec, ctxd is not the right choice today.
- **Vector search at scale.** ChromaDB and Qdrant are purpose-built vector databases with optimized indexes, quantization, and sharding. ctxd's vector view is in-memory HNSW rebuilt on restart -- fine for 10k vectors, not for 10M.
- **FTS at scale.** SQLite FTS5 is solid for small-to-medium datasets. For millions of documents, Elasticsearch or Typesense will be faster and more featureful (facets, fuzzy matching, etc.).
- **Distributed systems.** NATS has built-in clustering, geo-replication, and super-cluster federation. ctxd is single-node only until v0.3.
- **Ecosystem maturity.** Redis has 15+ years of production hardening, client libraries in every language, and a massive community. ctxd is v0.1.

## The honest take

ctxd is not trying to beat Redis at key-value storage or Qdrant at vector search. It occupies a different niche: a single substrate that gives AI agents persistent, tamper-evident, capability-scoped context over MCP.

The right comparison is: **what does it cost to assemble the same feature set from separate tools?** Redis + Elasticsearch + Qdrant + custom auth + MCP adapter layer + hash chain verification. That is the alternative ctxd replaces.

For a single developer or small team running AI agents locally, ctxd's performance is more than sufficient. The bottleneck in AI agent workflows is LLM inference (seconds per call), not context storage (microseconds per read).

## Running your own benchmarks

### Setup

```bash
cargo build --release
DB=/tmp/ctxd-bench.db
CTXD=./target/release/ctxd
```

### Write throughput

```bash
# Seed 10,000 events and measure wall time
rm -f $DB
time for i in $(seq 1 10000); do
  $CTXD --db $DB write \
    --subject "/bench/item-$i" \
    --type bench.write \
    --data "{\"i\":$i}" 2>/dev/null
done

# Note: CLI benchmark includes process spawn overhead (~10ms per invocation).
# Real throughput via wire protocol or in-process is 10-100x faster.
```

### Read latency

```bash
# After seeding events:
time $CTXD --db $DB read --subject /bench/item-500
time $CTXD --db $DB read --subject /bench --recursive
```

### FTS search

```bash
# Seed events with searchable content
for i in $(seq 1 1000); do
  $CTXD --db $DB write \
    --subject "/bench/docs/doc-$i" \
    --type bench.doc \
    --data "{\"content\":\"This is document $i about benchmarking performance\"}" 2>/dev/null
done

# Search
time $CTXD --db $DB query 'FROM e IN events WHERE e.subject LIKE "/bench/docs/%" PROJECT INTO e'
```

### Memory and DB size

```bash
# DB file size
ls -lh $DB

# Memory usage (start daemon, then check)
$CTXD --db $DB serve &
PID=$!
sleep 1
ps -o rss= -p $PID | awk '{print $1/1024 " MB"}'
kill $PID
```

### Capability token performance

```bash
# Mint
time $CTXD --db $DB grant --subject "/**" --operations "read,write,subjects,search"

# Verify
TOKEN=$($CTXD --db $DB grant --subject "/**" --operations "read")
time $CTXD --db $DB verify --token "$TOKEN" --subject /bench/item-1 --operation read
```

## Writing a Criterion benchmark

For reproducible in-process benchmarks, add to `crates/ctxd-store/Cargo.toml`:

```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["async_tokio"] }

[[bench]]
name = "store_bench"
harness = false
```

Then `crates/ctxd-store/benches/store_bench.rs`:

```rust
use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId};
use ctxd_core::{Event, Subject};
use ctxd_store::EventStore;
use tokio::runtime::Runtime;

fn bench_append(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let store = rt.block_on(EventStore::open_memory()).unwrap();

    c.bench_function("append_event", |b| {
        b.to_async(&rt).iter(|| async {
            let event = Event::new(
                "bench".to_string(),
                Subject::new("/bench/item").unwrap(),
                "bench.write".to_string(),
                serde_json::json!({"i": 1}),
            );
            store.append(event).await.unwrap();
        });
    });
}

fn bench_read(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let store = rt.block_on(EventStore::open_memory()).unwrap();

    // Seed 1000 events
    rt.block_on(async {
        for i in 0..1000 {
            let event = Event::new(
                "bench".to_string(),
                Subject::new(&format!("/bench/item-{i}")).unwrap(),
                "bench.write".to_string(),
                serde_json::json!({"i": i}),
            );
            store.append(event).await.unwrap();
        }
    });

    let subject = Subject::new("/bench").unwrap();
    c.bench_function("read_recursive_1000", |b| {
        b.to_async(&rt).iter(|| async {
            store.read(&subject, true).await.unwrap();
        });
    });
}

criterion_group!(benches, bench_append, bench_read);
criterion_main!(benches);
```

Run: `cargo bench -p ctxd-store`

## Benchmark reporting template

When publishing benchmark results, include:

```
Machine:     [CPU, RAM, disk type]
OS:          [name, version]
ctxd:        [version, commit hash]
Competitor:  [name, version]
Dataset:     [N events, avg event size, N subjects]
Methodology: [CLI / wire protocol / in-process / SDK]

Results:
  ctxd write:      X events/sec
  ctxd read:       X ms (exact), X ms (recursive, N events)
  ctxd search:     X ms (FTS, N events)
  ctxd vector:     X ms (k-NN, N vectors, D dimensions)
  competitor write: X events/sec
  competitor read:  X ms
  competitor search: X ms
```

## What ctxd is NOT competing on

Do not benchmark ctxd against:
- **LLMs** -- ctxd stores context, does not generate it
- **Agent frameworks** -- LangChain, CrewAI are orchestration layers
- **General databases** -- Postgres, DynamoDB are general-purpose
- **Search engines** -- Elasticsearch, Typesense are full-featured search

ctxd competes on the combination: event log + subject addressing + capability auth + MCP native + single binary. No single alternative does all five.
