# Benchmarking ctxd

How to measure ctxd's performance and compare it to alternatives.

## What to measure

ctxd competes in the "context for AI agents" space. The relevant benchmarks are:

| Metric | What it tells you | Target |
|--------|-------------------|--------|
| Write throughput | Events per second via append() | >10k events/sec on SQLite |
| Read latency (exact) | Time to read events for one subject | <1ms p99 |
| Read latency (recursive) | Time to read a subject tree | <10ms for 1000 events |
| FTS query latency | Time for a full-text search | <5ms for 100k events |
| Vector search latency | k-NN query time | <10ms for 10k vectors |
| MCP tool round-trip | End-to-end time for ctx_read over stdio | <50ms |
| Wire protocol round-trip | End-to-end time for PUB over TCP | <5ms |
| Cold start | Time from `ctxd serve` to first successful request | <500ms |
| Memory at rest | RSS with N events in the store | <50MB for 100k events |
| DB size per event | SQLite file growth per event | ~500B per event (JSON payload dependent) |
| Token mint latency | Time to mint a biscuit capability token | <1ms |
| Token verify latency | Time to verify a token | <1ms |

## Running benchmarks

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

# For higher throughput, use the wire protocol (avoids process spawn overhead):
# 1. Start daemon: $CTXD --db $DB serve &
# 2. Use a client that sends PUB over TCP in a loop
```

The CLI benchmark above includes process spawn overhead (~10ms per invocation). Real throughput via the wire protocol or in-process is 10-100x faster. To measure actual store throughput without process overhead:

```bash
# Rust benchmark (add to crates/ctxd-store/benches/)
cargo bench -p ctxd-store
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

## Comparing to alternatives

### Comparison matrix

| System | Category | Open source | Self-hosted | MCP native | Event log | Cap auth | Federation |
|--------|----------|-------------|-------------|------------|-----------|----------|------------|
| **ctxd** | Context substrate | Yes (Apache-2.0) | Yes | Yes | Yes | Yes (biscuit) | v0.3 |
| Mem0 | AI memory | Partial | No (cloud) | Partial | No | No | No |
| Zep | AI memory | Partial | Yes | Partial | Partial | No | No |
| Supermemory | AI memory | Partial | No | Yes | No | No | No |
| Letta (MemGPT) | Agent framework | Yes | Yes | No | No | No | No |
| ChromaDB | Vector DB | Yes | Yes | No | No | No | No |
| Qdrant | Vector DB | Yes | Yes | No | No | No | No |
| EventSourcingDB | Event store | No (commercial) | Yes | No | Yes | No (single token) | No |
| NATS | Message broker | Yes | Yes | No | Yes (JetStream) | Partial | Yes |

### What to benchmark against each

**Mem0** (https://mem0.ai)
- They have a Python SDK. Install: `pip install mem0ai`
- Compare: write latency (their `add()` vs ctxd `write`), search latency (their `search()` vs ctxd FTS), memory per fact stored
- Key difference: Mem0 requires an LLM for every write (extraction). ctxd stores raw events. Mem0 will be slower on writes but may return more structured data on reads.
- Their hosted tier has network latency. Compare against their self-hosted OSS version if possible.

```python
# Mem0 benchmark sketch
from mem0 import Memory
import time

m = Memory()
start = time.time()
for i in range(1000):
    m.add(f"Fact number {i} about benchmarking", user_id="bench")
write_time = time.time() - start
print(f"Mem0: {1000/write_time:.0f} writes/sec")

start = time.time()
for i in range(100):
    m.search("benchmarking", user_id="bench")
search_time = time.time() - start
print(f"Mem0: {100/search_time:.0f} searches/sec")
```

**Zep** (https://getzep.com)
- They have a Python SDK and a self-hosted Docker option.
- Compare: session memory write/read latency, fact extraction latency, search latency
- Key difference: Zep is session-oriented (conversations). ctxd is subject-oriented (paths). Different data models.

**ChromaDB / Qdrant** (vector DBs)
- Only compare vector search performance, since these are not context substrates.
- Use ctxd's vector view (user-supplied embeddings) vs their native vector insert/query.
- ctxd will be slower on vector operations (in-memory HNSW rebuilt on each insert). This is by design, ctxd is not a vector DB.

```python
# ChromaDB benchmark sketch
import chromadb
import time
import numpy as np

client = chromadb.Client()
collection = client.create_collection("bench")

embeddings = np.random.rand(1000, 384).tolist()
start = time.time()
collection.add(
    ids=[f"id-{i}" for i in range(1000)],
    embeddings=embeddings,
    documents=[f"Document {i}" for i in range(1000)]
)
print(f"ChromaDB insert 1000: {time.time()-start:.3f}s")

query = np.random.rand(384).tolist()
start = time.time()
for _ in range(100):
    collection.query(query_embeddings=[query], n_results=10)
print(f"ChromaDB 100 queries: {time.time()-start:.3f}s")
```

**EventSourcingDB** (https://eventsourcingdb.io)
- Commercial, free tier up to 25k events. Not open source.
- Compare: event append latency, subject-based read latency, hash chain verification
- Key difference: EventSourcingDB is a generic event store. ctxd adds capability auth, MCP, materialized views, and is OSS.
- Use their HTTP API for benchmarking.

**NATS** (https://nats.io)
- Compare message throughput (NATS PUB/SUB vs ctxd wire PUB/SUB), persistence (JetStream vs ctxd event log).
- NATS will win on raw message throughput (it's a message broker, not a context store). ctxd wins on context-specific features: subject-based read, FTS, capabilities, MCP.
- Use `nats bench` tool for NATS side.

```bash
# NATS benchmark (install nats CLI first)
nats bench test --pub 1 --msgs 10000 --size 256
```

### Benchmark reporting template

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

## What ctxd is NOT competing on

Do not benchmark ctxd against:
- **LLMs** (ctxd stores context, does not generate it)
- **Agent frameworks** (LangChain, CrewAI, etc. are orchestration layers)
- **General databases** (Postgres, Redis, DynamoDB are general-purpose)
- **Search engines** (Elasticsearch, Typesense are full-featured search)

ctxd competes on the combination: event log + subject addressing + capability auth + MCP native + single binary. No single alternative does all five.
