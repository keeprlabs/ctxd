# GitHub Adapter

Polls the GitHub REST API and publishes events into ctxd under
`/work/github/...`. Designed to fit the ctxd model: process-improvement signal,
not surveillance — engineering managers see what they would already see in
GitHub, captured into a single timeline so that 1:1s and reviews don't depend
on memory or recency bias.

Source: `crates/ctxd-adapter-github/`.

---

## 1. Personal Access Tokens

The adapter authenticates with a GitHub Personal Access Token (PAT). Both
fine-grained and classic PATs work; **fine-grained is recommended** because it
scopes credentials per repo.

### Fine-grained PAT (recommended)

1. <https://github.com/settings/personal-access-tokens/new>.
2. **Resource owner**: choose your user account, or an org if you have
   permissions to issue tokens for it.
3. **Repository access**: pick `Only select repositories` and list the repos
   you intend to poll. Use `All repositories` only if you need `--user` mode.
4. **Repository permissions** — the minimum the adapter needs:
   - `Contents`: Read-only
   - `Issues`: Read-only
   - `Pull requests`: Read-only
   - `Metadata`: Read-only (mandatory)
5. **Account permissions** (only if `--include-notifications` is set):
   - `Notifications`: Read-only
6. Set an expiry. Treat the token as a credential; it lives in the env or a
   secrets store, never in source.

### Classic PAT (fallback)

If fine-grained PATs aren't available, a classic token with the `repo` and
`notifications` scopes works the same way.

The adapter sends:

```
Authorization: Bearer <PAT>
X-GitHub-Api-Version: 2022-11-28
User-Agent: ctxd-adapter-github/0.3
Accept: application/vnd.github+json
```

The token is never logged. The `Authorization` header is marked sensitive on
the reqwest `HeaderValue`, and no debug/trace site prints `Config` or `RunArgs`
contents.

---

## 2. Running the adapter

The binary is `ctxd-adapter-github`. The two subcommands are `run` and
`status`.

### Single repo

```bash
ctxd-adapter-github run \
  --db ctxd.db \
  --token "$GITHUB_TOKEN" \
  --repo acme/web
```

### Multi-repo

`--repo` is repeatable:

```bash
ctxd-adapter-github run \
  --db ctxd.db \
  --token "$GITHUB_TOKEN" \
  --repo acme/web \
  --repo acme/api \
  --repo acme/infra
```

### User-wide

Polls every repo accessible to the token (via `GET /user/repos`). Mutually
exclusive with `--repo`:

```bash
ctxd-adapter-github run \
  --db ctxd.db \
  --token "$GITHUB_TOKEN" \
  --user
```

### Notifications

Notifications are on by default. To disable:

```bash
ctxd-adapter-github run \
  --db ctxd.db \
  --token "$GITHUB_TOKEN" \
  --user \
  --include-notifications=false
```

To poll only notifications:

```bash
ctxd-adapter-github run \
  --db ctxd.db \
  --token "$GITHUB_TOKEN" \
  --user \
  --kinds notifications
```

### `status`

Prints what's currently in the state DB without making any HTTP calls:

```bash
$ ctxd-adapter-github status
state_dir       : /Users/me/Library/Application Support/ctxd-adapter-github
last_poll_at    : 2026-04-24T10:14:07.512+00:00
rate_remaining  : 4982
rate_reset      : 1714000000
cursors:
  acme/web                       issues             since=2026-04-23T18:02:11Z
  acme/web                       pulls              since=2026-04-23T17:55:00Z
  acme/web                       issue_comments     since=2026-04-23T17:32:00Z
  acme/web                       pr_comments        since=2026-04-22T23:00:00Z
  user                           notifications      since=2026-04-23T18:00:00Z
```

---

## 3. Subject scheme

Every event published by this adapter lives under `/work/github`:

| Resource | Subject |
|---|---|
| Issue | `/work/github/{owner}/{repo}/issues/{number}` |
| PR | `/work/github/{owner}/{repo}/pulls/{number}` |
| Issue comment | `/work/github/{owner}/{repo}/issues/{number}/comments/{id}` |
| PR review comment | `/work/github/{owner}/{repo}/pulls/{number}/comments/{id}` |
| Notification | `/work/github/notifications/{id}` |

Event types:

| Resource | First time we see it | Subsequent updates | Closed | Merged |
|---|---|---|---|---|
| Issue | `issue.opened` | `issue.updated` | `issue.closed` | — |
| PR | `pr.opened` | `pr.updated` | `pr.closed` | `pr.merged` |
| Comment | `comment.created` | `comment.updated` | — | — |
| Notification | `notification.received` | (treated as new) | — | — |

v0.3 distinguishes "new" from "updated" by checking whether the resource is in
the `seen_resources` table (see DB layout below). It does **not** diff the
prior state to figure out which fields changed. That's a deliberate v0.3
limitation: if you need fine-grained transition events, layer them on top of
the event log in a downstream materialized view.

### Event payload shape

Issue / PR (pruned from the GitHub object — bodies truncated to 16 KiB):

```json
{
  "owner": "acme",
  "repo": "web",
  "number": 42,
  "title": "Login race condition",
  "body": "Repro steps…",
  "body_full_size": 1283,
  "state": "open",
  "author": { "login": "alice", "id": 1, "type": "User" },
  "labels": [...],
  "assignees": [...],
  "milestone": null,
  "created_at": "2026-04-01T00:00:00Z",
  "updated_at": "2026-04-23T10:00:00Z",
  "closed_at": null,
  "html_url": "https://github.com/acme/web/issues/42"
}
```

PR-only fields: `merged`, `merged_at`, `merge_commit_sha`, `head`, `base`.

Comment:

```json
{
  "owner": "acme", "repo": "web",
  "id": 1001, "parent_kind": "issue", "parent_number": 42,
  "body": "looks good", "body_full_size": 10,
  "author": { "login": "bob", "id": 2, "type": "User" },
  "created_at": "...", "updated_at": "...",
  "html_url": "..."
}
```

Notification:

```json
{
  "id": "12345", "reason": "subscribed",
  "subject": { "title": "...", "type": "PullRequest", "url": "..." },
  "repository": { "full_name": "acme/web" },
  "updated_at": "...", "unread": true
}
```

If a body exceeds 16 KiB, it's truncated at a UTF-8 char boundary and a single
`…` is appended; the original size is recorded in `body_full_size` so
consumers can detect truncation.

---

## 4. Rate-limit behavior

GitHub's REST API is rate-limited. The adapter watches the
`X-RateLimit-Limit`, `X-RateLimit-Remaining`, and `X-RateLimit-Reset` headers
on every response and reacts as follows:

| Condition | Behavior |
|---|---|
| `remaining > 10% of limit` | Continue immediately. |
| `remaining ≤ max(1, 10% of limit)` and reset is in the future | Sleep until `reset_unix + 1s`. Logged at WARN level. |
| `429 Too Many Requests` | Honor `Retry-After` header (seconds). Retries up to 3 times. |
| `403` with `X-RateLimit-Remaining: 0` and `Retry-After` set (secondary rate limit) | Same as 429. |
| `5xx` | Exponential backoff with 25% jitter, base 250ms, capped at 30s. Retries up to 3 times. |
| Network timeout | Same backoff schedule as 5xx. |

`ETag` revalidation also reduces quota: every poll includes
`If-None-Match: <etag>`, and a `304 Not Modified` response **does not** count
against the rate-limit budget per GitHub's docs.

---

## 5. State DB layout

State lives in `<state-dir>/github.state.db` (SQLite via `sqlx`).
Default `state-dir`:

| Platform | Path |
|---|---|
| Linux | `$XDG_STATE_HOME/ctxd-adapter-github` (defaults to `~/.local/state/ctxd-adapter-github`) |
| macOS | `~/Library/Application Support/ctxd-adapter-github` |
| Windows | `{FOLDERID_LocalAppData}\ctxd-adapter-github` |
| Fallback | `./.ctxd-adapter-github` |

Schema:

```sql
CREATE TABLE etags (
  url        TEXT PRIMARY KEY,   -- request URL with the query string stripped
  etag       TEXT NOT NULL,      -- value sent next time as If-None-Match
  fetched_at TEXT NOT NULL
);

CREATE TABLE cursors (
  scope TEXT,                    -- "owner/repo" or "user"
  kind  TEXT,                    -- "issues" | "pulls" | "issue_comments" | ...
  since TEXT NOT NULL,           -- highest updated_at seen
  PRIMARY KEY (scope, kind)
);

CREATE TABLE seen_resources (
  kind            TEXT,          -- "issue" | "pr" | "issue_comment" | "pr_comment" | "notification"
  resource_key    TEXT,          -- "owner/repo/123" for issues/PRs, etc.
  last_updated_at TEXT NOT NULL,
  last_state      TEXT,          -- nullable; only relevant for issues + PRs
  PRIMARY KEY (kind, resource_key)
);

CREATE TABLE poll_meta (
  key   TEXT PRIMARY KEY,        -- "last_poll_at", "rate_remaining", "rate_reset"
  value TEXT NOT NULL
);
```

The `etag` table keys on URL **path only** (no query string). That's
deliberate: when the `since=` cursor advances each poll, the URL changes too,
which would invalidate every ETag. Keying by path lets a 304 short-circuit a
poll where nothing relevant changed.

Idempotency: the adapter compares each item's `updated_at` against
`seen_resources.last_updated_at` and skips items that haven't moved forward.
That means restarting the adapter mid-stream — even after a crash — never
re-publishes events that have already been emitted.

---

## 6. Troubleshooting

**Token rejected (401 / 403)**:

- Re-check scopes (especially fine-grained PATs — each repo must be in the
  selected list).
- For org-owned repos, the org may require SSO authorization on the token —
  go to <https://github.com/settings/tokens> and click "Configure SSO".

**Notifications endpoint returns 401**:

- Add the `Notifications: Read-only` permission to your fine-grained PAT, or
  use a classic PAT with the `notifications` scope.

**Adapter sleeps for a long time on startup**:

- That's the rate-limit pause. Check
  `ctxd-adapter-github status` — if `rate_remaining` is small, the previous
  run consumed quota. The pause ends at the listed `rate_reset`.

**Repeated `pr_comment_subject` events with id=0**:

- Some PR review comments don't have a `pull_request_url` (legacy data).
  These are silently skipped. If you suspect a real bug, run with
  `RUST_LOG=ctxd_adapter_github=debug` and grep for `skip:`.

**State got corrupted**:

- Delete `<state-dir>/github.state.db`. Next poll will treat everything as
  new and re-publish issues/PRs/comments at their current state. The
  ctxd event store will see this as a flood of `*.opened` events;
  downstream materialized views are LWW-by-`(updated_at, id)` so the
  end state remains correct, but you may see duplicates.

---

## 7. Internals quick map

| File | Responsibility |
|---|---|
| `src/config.rs` | `Config`, `RepoSelector`, `RepoRef`, `ResourceKind`. |
| `src/state.rs` | `StateDb` — sqlx-sqlite, schema migrations on open. |
| `src/client.rs` | `GhClient` — auth headers, rate-limit, ETag, retries, pagination. |
| `src/parse.rs` | `Link` header + `Retry-After` parsing. |
| `src/events.rs` | Subject formatting, payload pruning, body truncation, event-type classification. |
| `src/poller.rs` | Main polling loop — issues, PRs, comments, notifications. |
| `src/adapter.rs` | `Adapter` trait impl that ties everything together. |
| `src/main.rs` | clap CLI — `run` + `status`. |
| `tests/*.rs` | wiremock-driven integration tests, one file per scenario. |
