# Gmail adapter

Status: shipped in v0.3 (4C).

The Gmail adapter ingests messages from a Gmail inbox into ctxd as
typed events. It is a long-running daemon that periodically polls
Gmail's History API and publishes one event per `(message, label)`
pair under the `/work/email/gmail/` subject tree.

## What it does

- Authorizes once via OAuth2 device-code flow. No browser or callback
  server is required on the host running the adapter.
- Encrypts the resulting refresh token at rest with AES-256-GCM under
  a key derived from a master key in the same state directory.
- Polls `users.history.list` on a configurable interval. On the first
  run (no cursor), it does a full sync via `users.messages.list` and
  records the current `historyId`.
- Falls back to a full sync automatically if the cursor expires (Gmail
  returns 404 once the historyId is older than the retention window —
  see [Troubleshooting](#troubleshooting)).
- Publishes one CloudEvents-shaped event per `(message, label)` pair
  with a normalized subject path.
- Idempotent across restarts and re-polls — a small SQLite file in the
  state directory records which `(gmail_internal_id, label)` pairs
  have been published.
- Honors `Retry-After` headers and applies exponential backoff with
  jitter on 429 / 5xx.

## What it doesn't do

- Push notifications via Pub/Sub. Polling is the v0.3 strategy. Push
  is a v0.4 candidate.
- Attachments. Headers + body text only. Body capped at 128 KB.
- HTML rendering. The body extractor prefers `text/plain` and falls
  back to a small `text/html` → text stripper that drops `<script>`
  / `<style>` blocks and tags. It is **not** a full HTML renderer.
- Send. The adapter requests
  `https://www.googleapis.com/auth/gmail.readonly` scope only.
- Multi-account. One adapter process serves one Gmail user. Run
  multiple processes with different `--state-dir`s for multi-account.

## Required scopes

```
https://www.googleapis.com/auth/gmail.readonly
```

That's it. We don't need `gmail.modify`, `gmail.metadata`, or
`gmail.send`.

## Setting up a Google Cloud OAuth client

1. Open the Google Cloud Console and select (or create) a project.
2. Go to **APIs & Services → Library** and enable **Gmail API**.
3. Go to **APIs & Services → OAuth consent screen**:
    - User type: **External** (or Internal if you're on a Workspace
      tenant and only your tenant will use this).
    - Add the scope `https://www.googleapis.com/auth/gmail.readonly`.
    - In testing mode, add yourself (and any other test accounts) as
      test users.
4. Go to **APIs & Services → Credentials → Create credentials → OAuth
   client ID**:
    - Application type: **TVs and Limited Input devices**.
    - Note the **Client ID** and **Client secret** that appear in the
      modal. The client secret is treated as a low-trust value by the
      device flow — it's only used to identify the client to Google,
      not to authenticate the user.

You will pass these via `--client-id` / `--client-secret` flags or
the `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` environment variables.
The adapter never persists the client secret to disk.

## Authorize flow walkthrough

```bash
export GOOGLE_CLIENT_ID="<your-client-id>"
export GOOGLE_CLIENT_SECRET="<your-client-secret>"

ctxd-adapter-gmail auth
```

The adapter prints the verification URL and a short user code to
stderr:

```
==============================================
Open this URL in a browser:
  https://www.google.com/device
Enter this code:
  ABCD-EFGH
==============================================
(waiting for authorization...)
```

Open the URL on any device that has a browser, type the code, and
sign in with the Google account whose mailbox you want to ingest.
The adapter polls Google's token endpoint until it returns the
refresh token, then encrypts and writes it to disk.

## Running the sync loop

```bash
ctxd-adapter-gmail run \
  --db ./ctxd.db \
  --state-dir ~/.local/state/ctxd-adapter-gmail \
  --user-id me \
  --labels INBOX,SENT \
  --poll-interval 60s
```

Flags:

- `--db` — path to the local ctxd SQLite database to publish into. In
  v0.3 this is required; remote-mode (`--ctxd-url`) is reserved for
  the federation wire protocol and not yet wired.
- `--state-dir` — directory holding the master key, encrypted refresh
  token, and SQLite state DB. Defaults to
  `$XDG_STATE_HOME/ctxd-adapter-gmail` or
  `~/.local/state/ctxd-adapter-gmail`.
- `--user-id` — Gmail user id, almost always `me`.
- `--labels` — comma-separated list of Gmail labels to sync. Defaults
  to `INBOX,SENT`.
- `--poll-interval` — interval between polls. Accepts `60s`, `5m`,
  `1h`. Plain integers are seconds.
- `--client-id` / `--client-secret` — OAuth credentials, also
  available via env. Used for refreshing the access token on each
  poll.

## Status

```bash
ctxd-adapter-gmail status --state-dir ~/.local/state/ctxd-adapter-gmail
```

Prints the paths it would use, whether the encrypted token is
present, the last `historyId` we synced from, the last poll
timestamp, and how many `(message, label)` pairs we've published.

## Subject naming scheme

```
/work/email/gmail/{label}/{message_id}
```

- `label` is normalized: lowercased, `/` and special characters
  replaced with `-`, runs collapsed.
    - `INBOX` → `inbox`
    - `SENT` → `sent`
    - `Projects/Sagework` → `projects-sagework`
    - `[Imap]/Sent` → `imap-sent`
- `message_id` is the Gmail-internal id (the `id` field on the
  message resource), not the RFC 822 `Message-ID` header.

When a message has multiple labels (e.g., `INBOX` + `IMPORTANT`), the
adapter publishes **one event per label**. Each event carries the
full `labels` array in `data` so consumers can correlate. This trades
storage for query simplicity — searching for "all important emails"
or "everything in INBOX" is a single subject-prefix query, no JOIN.
The idempotency table is keyed on `(gmail_internal_id, label)` so
duplicate events are never published.

## Event types

- `email.received` — default for messages without `SENT` or `DRAFT`
  labels.
- `email.sent` — message has the `SENT` label and not `DRAFT`.
- `email.draft` — message has the `DRAFT` label (takes priority over
  `SENT`).

## Event data shape

```json
{
  "from": "alice@example.com",
  "to": ["me@example.com"],
  "cc": [],
  "bcc": [],
  "subject": "Hello",
  "snippet": "Hi there...",
  "body": "Hi there, just checking in...",
  "date_rfc3339": "2024-04-01T12:00:00+00:00",
  "message_id": "<abc@example.com>",
  "thread_id": "1893def0123",
  "labels": ["INBOX", "IMPORTANT", "UNREAD"],
  "list_id": null,
  "gmail_internal_id": "1893def0456"
}
```

- `from` is the raw `From:` header (display name + address).
- `to`, `cc`, `bcc` are split on commas. Quoted display names with
  embedded commas are left as a single string in the array — Gmail's
  metadata endpoint pre-renders these correctly in practice.
- `body` is `text/plain` if present, else `text/html` stripped to
  text. Capped at 128 KB. Empty when the message has no parseable
  body.
- `list_id` is the `List-Id:` header (mailing-list signal). Present
  on most newsletters and group emails.
- `gmail_internal_id` is the same id used in the subject path. It
  is stable across labels.

## Token storage and encryption scheme

State directory layout:

```
<state-dir>/
├── gmail.key         # 32 random bytes (file mode 0600)
├── gmail.token.enc   # encrypted refresh token (file mode 0600)
└── gmail.state.db    # SQLite: cursor + idempotency table
```

The encrypted token file layout is:

```
| salt (16 B) | nonce (12 B) | ciphertext + AEAD tag |
```

- **Master key** lives in `gmail.key`. Generated on first `auth`
  call. 32 random bytes from `OsRng`. Whoever can read this file can
  decrypt the token; protect the state directory accordingly.
- **Per-write key derivation**: HKDF-SHA256 with a fresh random salt
  per write. Domain-separated by an info string
  (`ctxd-adapter-gmail/v1/aes-256-gcm`).
- **AEAD AAD**: bound to the file format
  (`ctxd-adapter-gmail/v1/token`). A ciphertext from a different
  field can't be substituted.
- **Nonce**: 12 random bytes per write. The (key, nonce) pair is
  fresh on every encryption so the AES-GCM nonce-reuse pitfall is
  structurally avoided.

The access token is **never persisted**. It's short-lived (1 hour)
and re-fetched at the start of every `run` and whenever a sync loop
detects it expires within 60 seconds.

## Rate limiting

The adapter shares a single connection-pooled `reqwest::Client`
across the lifetime of the process. Per-message fetches use a
`tokio::sync::Semaphore` capped at 10 concurrent in-flight
`messages.get` calls.

When Gmail returns 429 or 5xx:

1. If a `Retry-After` header is present, the adapter sleeps for
   exactly that many seconds.
2. Otherwise it applies exponential backoff with full jitter:
   `rand(0..min(base * 2^attempt, cap))` where base=250ms and
   cap=30s.
3. After 6 attempts (default `max_retries=5`), the request fails
   with `GmailError::RetriesExhausted`. The sync iteration logs and
   continues; the next poll will retry.

## Troubleshooting

### "history cursor expired"

Gmail's History API only retains a few days of history. If the
adapter is offline for longer than that, the next poll will return
404 and the adapter will fall back to a full `messages.list` sync,
recording the new `historyId` from `users.getProfile`. This is
normal and self-healing — there's nothing to do.

You'll see a log line like:

```
WARN history cursor expired; falling back to full sync
```

### "rate limit"

Repeated `WARN gmail request failed; retrying` lines mean Gmail is
rate-limiting the adapter. The exponential-backoff loop will absorb
short bursts. If it persists, lower the polling frequency
(`--poll-interval 5m`) or the fetch concurrency (recompile with a
smaller `DEFAULT_FETCH_CONCURRENCY`). Workspace tenants have higher
quotas than personal accounts.

### "reauthorization needed"

If the user revokes the OAuth grant in
[Google Account → Security → Third-party apps](https://myaccount.google.com/connections),
the next refresh will return 400 `invalid_grant`. The adapter logs
the failure and exits the sync iteration. Re-run `auth` to obtain a
fresh refresh token.

The same applies if the OAuth client is deleted in the Google Cloud
Console.

### "decryption failed"

Means the master key in `gmail.key` doesn't match the encrypted
token. This happens if you copy `gmail.token.enc` between machines
without copying `gmail.key`. The two files form a pair — never copy
one without the other.

### "ENOENT loading token"

Means `gmail.token.enc` doesn't exist yet — run `auth` first.

## Security notes

- Client secret, refresh token, and access token are never logged.
  `tracing` calls in the adapter never carry token content.
- The encrypted token file uses AEAD, so a tampered ciphertext or
  tag fails decryption with an authentication error rather than
  silently producing garbage.
- The state directory should live on local storage. Putting it on
  network-mounted storage that doesn't honor Unix permissions
  (e.g., SMB) defeats the 0600 file mode.
- The capability flag (`--cap`) is reserved for the federation wire
  path and is currently ignored when `--db` is set. Local-mode
  publication writes directly into the SQLite store the operator
  controls.
