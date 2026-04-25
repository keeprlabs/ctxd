# ADR 015: Hybrid search via Reciprocal Rank Fusion

Status: Accepted (v0.3 Phase 4B)
Date: 2026-04-24

## Context

`ctx_search` ships three modes:

- `fts` — exact-token search backed by SQLite's FTS5.
- `vector` — k-NN over the persisted HNSW index.
- `hybrid` — combine both for better recall.

Pure FTS misses semantic matches (synonyms, paraphrases). Pure
vector misses exact-token matches when the corpus contains
identifiers or domain-specific terms the embedder hasn't seen.
Hybrid solves both — but how do we *combine* the rankings?

## Options considered

### Option A — Score normalization + weighted sum

For each hit, normalize the FTS BM25 score and the vector cosine
distance to `[0, 1]`, then combine with a tunable weight `α`:

```
combined = α × normalized_fts + (1 - α) × normalized_vector
```

Pros: numerically intuitive; easy to bias toward one side.
Cons: BM25 and cosine live in incompatible scales; the
distribution of scores is corpus-dependent, so a fixed `α` works
for one corpus and not another. Tuning `α` is a per-deployment
chore.

### Option B — Reciprocal Rank Fusion (RRF)

For each hit, sum `1 / (k + rank_i)` across every result list it
appears in. Sort descending. Take top k.

Pros: scale-free — only ranks matter, not raw scores. Robust
across corpora. Has a 15+ year track record (Cormack et al. 2009,
TREC). Adopted by Elastic, Vespa, Anserini, Vespa.ai.
Cons: throws away score magnitudes — two docs at the same rank
contribute equally regardless of how confidently they were
ranked.

### Option C — Learning-to-rank

Train a model to fuse FTS + vector signals. Best quality. Not
shippable in a single-node SDK without operational complexity
we don't want in v0.3.

## Decision

Use Option B (RRF). The 15-year lit + industry consensus is hard
to argue with; we don't have the per-corpus calibration story
that Option A would require, and Option C is several years of
roadmap away.

### The k constant

The RRF formula is `1 / (k + rank)`. The original 2009 paper used
`k = 60`. Every major implementation since has kept it: Elastic,
Vespa, Anserini, OpenSearch. We do too. The value is documented
both in code (`RRF_K_CONST = 60.0`) and here so the choice is
auditable.

Why 60 and not something else? It bounds the score contribution
from very low ranks. At rank 1 a list contributes `1/61`; at rank
100 it contributes `1/160`. A smaller k (say 10) would let rank-1
dominate too aggressively — perfectly fine if you trust your
rankers, but RRF's appeal is precisely that it works when neither
ranker is fully trusted. 60 is the conservative middle ground.

### Default mode

`ctx_search` defaults to `hybrid` when an embedder is configured,
`fts` otherwise. Rationale:

- An operator who wired up an embedder presumably wants to use
  it. Forcing them to pass `search_mode: "hybrid"` every time
  would be needless ceremony.
- Without an embedder, vector and hybrid both require an embed
  call we can't make — degrading to FTS preserves the v0.2
  user-visible behavior.

## Implementation

```rust
fn reciprocal_rank_fusion(lists: &[&[String]], k: usize) -> Vec<String> {
    let mut scores: HashMap<&str, f32> = HashMap::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            *scores.entry(id.as_str()).or_insert(0.0) +=
                1.0 / (60.0 + (rank as f32 + 1.0));
        }
    }
    // sort by score desc, tiebreak by id for determinism, take k.
}
```

Ranks are 0-indexed in our slice but 1-indexed in the RRF formula,
matching the paper. Tiebreak by id so identical scores produce
deterministic output across runs.

## Consequences

- Hybrid latency ≈ FTS latency + vector latency + a constant-time
  HashMap fold (<50 µs at typical k). Confirmed by the
  `hybrid_fts_plus_vector_k10_n10k` bench.
- We over-pull from each side (`pull = max(20, 4×k)`) so RRF has
  enough candidates to find documents in both lists.
- Adding a third source (e.g. graph hits from `ctx_entities`) is
  trivial — just append another list to `lists`.

## Revisit when

- A user reports that `k = 60` is biased against their corpus.
  Make it configurable on `ctx_search` if so.
- We ship a learning-to-rank fuser (probably v0.5).
- Vector + FTS scores get calibrated such that Option A becomes
  viable. Not a near-term concern.
