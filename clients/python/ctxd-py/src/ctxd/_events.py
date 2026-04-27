"""CloudEvents-shaped :class:`Event` model + canonical-bytes helper.

The :class:`Event` shape mirrors ``crates/ctxd-core/src/event.rs`` and
the JSON Schema at ``docs/api/events.schema.json``. We keep IDs as
plain strings (UUIDv7 lexical form) to guarantee byte-exact round-trip
through pydantic — coercing to ``uuid.UUID`` would normalize to
canonical lowercase and risk drifting from the wire encoding the
daemon emits.

Optional fields use ``model_config["json_schema_extra"]`` semantics: a
field that is ``None`` / empty list is omitted on the wire, but always
serialized in the canonical form used for hashing and signing.
"""

from __future__ import annotations

import json
from datetime import datetime
from typing import Any

from pydantic import BaseModel, ConfigDict, Field, field_serializer


class Event(BaseModel):
    """A CloudEvents v1.0 event with ctxd extensions.

    ``event_type`` is the Python field name; on the wire it serializes
    as ``type`` per the CloudEvents spec. Optional fields are omitted
    from the JSON output when absent / empty.
    """

    model_config = ConfigDict(
        # Allow ``type`` to populate ``event_type`` on parse, and the
        # serializer alias to emit ``type`` on dump.
        populate_by_name=True,
        # We use ``additionalProperties: false`` semantics on the wire,
        # but accept unknown fields silently to forward-compat with new
        # daemon releases.
        extra="ignore",
    )

    specversion: str = "1.0"
    """CloudEvents spec version. Always ``"1.0"`` for v0.3."""

    id: str
    """Globally-unique UUIDv7 string."""

    source: str
    """Identifies the context in which the event happened."""

    subject: str
    """Subject path the event is filed under."""

    event_type: str = Field(alias="type")
    """Event type discriminator (e.g. ``ctx.note``)."""

    time: datetime
    """RFC3339 timestamp of when the event was created."""

    datacontenttype: str = "application/json"
    """Content type of ``data``."""

    data: Any
    """Event payload. JSON of any shape."""

    predecessorhash: str | None = None
    """SHA-256 hash of the predecessor event's canonical form."""

    signature: str | None = None
    """Ed25519 signature over the canonical form, hex-encoded."""

    parents: list[str] = Field(default_factory=list)
    """Parent event ids (UUIDv7 strings)."""

    attestation: str | None = None
    """Optional TEE attestation blob, hex-encoded."""

    @field_serializer("time")
    def _serialize_time(self, t: datetime) -> str:
        """Serialize time as RFC3339 with ``Z`` suffix when UTC.

        The Rust daemon emits ``2026-01-01T00:00:00Z`` (no offset
        suffix) for UTC times. We match that shape so a round-trip of a
        daemon-produced event is byte-stable.
        """
        return _format_rfc3339(t)

    def model_dump_wire(self) -> dict[str, Any]:
        """Dump the event in wire form (omitting empty optional fields).

        - ``predecessorhash``, ``signature``, ``attestation`` are
          omitted when ``None``.
        - ``parents`` is omitted when empty (matches the Rust
          ``skip_serializing_if = "Vec::is_empty"``).
        """
        out: dict[str, Any] = {
            "specversion": self.specversion,
            "id": self.id,
            "source": self.source,
            "subject": self.subject,
            "type": self.event_type,
            "time": _format_rfc3339(self.time),
            "datacontenttype": self.datacontenttype,
            "data": self.data,
        }
        if self.predecessorhash is not None:
            out["predecessorhash"] = self.predecessorhash
        if self.signature is not None:
            out["signature"] = self.signature
        if self.parents:
            out["parents"] = list(self.parents)
        if self.attestation is not None:
            out["attestation"] = self.attestation
        return out


def _format_rfc3339(t: datetime) -> str:
    """Emit ``YYYY-MM-DDTHH:MM:SS[.fff]Z`` for UTC, keep offset for non-UTC.

    The daemon serializes ``DateTime<Utc>`` via chrono, which emits
    ``Z`` for UTC and the offset for other timezones. Python's
    ``datetime.isoformat`` emits ``+00:00`` for UTC by default — we
    convert that to ``Z`` so round-trips are byte-stable.
    """
    if t.tzinfo is None:
        # Treat naive datetimes as UTC. The daemon never emits naive
        # times; this branch only protects callers that hand-build
        # Events without a tzinfo.
        s = t.isoformat() + "Z"
        return s
    iso = t.isoformat()
    if iso.endswith("+00:00"):
        return iso[:-6] + "Z"
    return iso


def canonical_bytes(event: Event) -> bytes:
    """Produce the canonical signing bytes for ``event``.

    Mirrors ``ctxd_core::signing::canonical_bytes`` exactly: a JSON
    object with **sorted keys** containing every CloudEvents field
    except ``predecessorhash`` and ``signature``, plus the v0.3
    ``parents`` (sorted lexicographically by string id; empty array if
    none) and ``attestation`` (hex-encoded or ``null``) fields.

    The output is `serde_json::to_vec(BTreeMap<&str, Value>)` on the
    Rust side: compact JSON (no spaces) with sorted keys.
    """
    parents_sorted = sorted(event.parents)
    attestation_val: str | None = event.attestation  # already hex string
    payload = {
        "attestation": attestation_val,
        "data": event.data,
        "datacontenttype": event.datacontenttype,
        "id": event.id,
        "parents": parents_sorted,
        "source": event.source,
        "specversion": event.specversion,
        "subject": event.subject,
        "time": _format_rfc3339(event.time),
        "type": event.event_type,
    }
    # ``sort_keys=True`` matches BTreeMap iteration order on the Rust
    # side. ``separators=(",", ":")`` matches serde_json's compact
    # output (no whitespace).
    return json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
