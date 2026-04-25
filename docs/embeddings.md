# Embeddings + vector search

Phase 4 of v0.3 introduces real embedders, a persisted HNSW
vector index, and hybrid search. This guide is for operators
who want to turn it on, and for engineers debugging it when
something goes wrong.

## Pick a backend

ctxd ships three embedder backends:

| Provider | Feature flag | Default model | Dim | Auth | Best for |
|----------|--------------|---------------|-----|------|----------|
| `null` | always on | `null-embedder` | 384 | — | Tests, dev, "embedder not configured" |
| `openai` | `openai` | `text-embedding-3-small` | 1536 | API key | Cloud-hosted production |
| `ollama` | `ollama` | `nomic-embed-text` | 768 | none | Local + privacy-preserving |

Pick `null` when you want the trait surface but not the IO
(`ctx_search` defaults to `fts` mode). Pick `openai` when you
already have an OpenAI key and you want quality + zero ops.
Pick `ollama` when the data must stay on the box.

The CLI defaults to building both `openai` and `ollama` features
in. Disable either with `--no-default-features --features openai`
when you need a slimmer binary.

## Configure at startup

The relevant flags on `ctxd serve`:

```
--embedder {null|openai|ollama}      Provider. Default: null.
--embedder-model <STR>               Override model (provider-specific default).
--embedder-url <STR>                 Override base URL (default depends on provider).
--embedder-api-key <STR>             API key (OpenAI only). Falls back to OPENAI_API_KEY env.
```

Examples:

```bash
# OpenAI (env key)
export OPENAI_API_KEY=sk-...
ctxd serve --embedder openai

# OpenAI with a non-default model
ctxd serve --embedder openai --embedder-model text-embedding-3-large

# Local Ollama on default port
ollama pull nomic-embed-text
ctxd serve --embedder ollama

# Self-hosted OpenAI-compatible proxy
ctxd serve --embedder openai \
  --embedder-url https://embed.internal.example.com/v1 \
  --embedder-api-key "$INTERNAL_API_KEY"
```

API keys are never logged or echoed in error messages. If you
see `sk-` show up in `journalctl`, file a security bug.

## What gets embedded

When an embedder is configured, `EventStore::append` (called by
`ctx_write`, the wire protocol's `pub`, and the federation
inbound path) auto-embeds events that have indexable text:

- The event's subject (always included as a semantic anchor).
- All top-level string, number, and boolean values from the
  event's JSON payload.

Embedding failures log a warning but **do not** fail the append.
Embeddings are a materialized view; the event log is the source
of truth.

## Search modes

`ctx_search` accepts a `search_mode` parameter:

- `fts` — full-text search via SQLite FTS5. Always available.
- `vector` — embed the query, k-NN over the HNSW index. Requires
  an embedder.
- `hybrid` — both, fused with Reciprocal Rank Fusion. Requires an
  embedder.

The default is `hybrid` when an embedder is configured, `fts`
otherwise. So once you've started `ctxd serve --embedder openai`,
all your `ctx_search` calls get hybrid for free.

Example:

```jsonc
{
  "tool": "ctx_search",
  "params": {
    "query": "performance review draft for Q1",
    "k": 10,
    "search_mode": "hybrid"
  }
}
```

## The HNSW index

The vector index lives next to the SQLite database file. If your
`--db` is `ctxd.db`, you'll see four sidecar files appear after
the first auto-flush:

- `ctxd.db.hnsw.graph` — `hnsw_rs` adjacency lists.
- `ctxd.db.hnsw.data` — vector payloads.
- `ctxd.db.hnsw.meta` — ctxd magic header + dim + element_count.
- `ctxd.db.hnsw.map` — internal-id → event_id mapping.

These are *materialized*: lose them and the next startup rebuilds
from `vector_embeddings`.

### Tuning

ctxd uses HNSW parameters `M=16`, `ef_construction=200`,
`ef_search=50`. See `docs/decisions/014-hnsw-persistence.md`
for rationale. We don't currently expose these on the CLI —
the defaults are sized for ≤1M vectors per node, which covers
every v0.3 deployment. If you have a workload that breaks
that, open an issue.

### Flush cadence

The graph is dumped to disk:

1. On graceful shutdown.
2. Every 1000 inserts (configurable via
   `VectorIndexConfig::flush_every_n_inserts` if you embed ctxd
   as a library).

A hard kill between flushes loses the most recent 0–1000
in-memory inserts from the on-disk graph. On next startup, the
mismatch between the on-disk element count and the
`vector_embeddings` row count triggers a rebuild from SQL.

## Rebuilding the index

Three triggers, in order of likelihood:

1. **Sidecar files missing.** First start, or you nuked them on
   purpose. Just restart the daemon — `ensure_vector_index`
   does the rebuild.
2. **Magic-number mismatch in `<db>.hnsw.meta`.** Indicates a
   torn write or version skew. Restart triggers rebuild.
3. **Element-count mismatch.** Indicates an unclean prior shutdown.
   Restart triggers rebuild.

To force a rebuild from scratch:

```bash
rm ctxd.db.hnsw.*
ctxd serve --embedder openai
# the daemon logs:
#   INFO ctxd_store_sqlite::store: rebuilding HNSW vector index from vector_embeddings
#   INFO ctxd_store_sqlite::views::vector: vector index rebuild complete
```

For very large stores (>100k vectors) the rebuild emits progress
every 10k entries. If you don't see progress in 60 s on a
non-trivial corpus, file a bug — that means we're stuck on a
single CPU and need parallel insert.

## Dimension mismatch

If you swap embedders without dropping the old index, the next
`ensure_vector_index` call fails the dimension check, logs a
warning, and rebuilds with the new model. The old vectors in
`vector_embeddings` are *kept* (we never delete from the source
of truth) but skipped during rebuild — they're effectively
stale until you re-index them with the new model.

There's no automatic re-embed of historical events on model
change. To force one, walk the events you care about and call
`ctx_write` with the same payload (idempotent on event id).

## Troubleshooting

**"OPENAI_API_KEY not set"** — set the env var or pass
`--embedder-api-key`.

**"openai status 401"** — bad key. We never echo the key into
the error message, so check your env or `--embedder-api-key`
spelling locally.

**"openai status 429"** — rate-limited. The client retries 3
times with exponential backoff, honoring `Retry-After` if
present. If it still fails, you're consistently over quota;
either upgrade or fall back to Ollama.

**"connection refused" when using ollama** — Ollama isn't
running on the configured `--embedder-url`. Default is
`http://localhost:11434`. Start it: `ollama serve`.

**Search results look wrong after a model swap** — see the
"Dimension mismatch" section. You probably need to re-embed
historical events.

**`ctxd.db.hnsw.*` files keep appearing then disappearing** —
two `ctxd` instances are racing on the same database. The
intended deployment is single-writer per database; if you need
multiple readers consult the federation docs instead.
