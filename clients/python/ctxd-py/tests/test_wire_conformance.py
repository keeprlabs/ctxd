"""Wire-protocol conformance tests against ``docs/api/conformance/wire/``.

The shipped JSON fixtures use the Rust *struct* shape with named
fields (e.g. ``{"Pub": {"subject": ..., "event_type": ..., "data":
...}}``). On the wire, ``rmp-serde`` encodes those as positional
arrays in field declaration order. The Python SDK matches that shape;
this test asserts the bytes match exactly.

Field declaration order (from
``crates/ctxd-wire/src/messages.rs``):

- ``Pub``: subject, event_type, data
- ``Sub``: subject_pattern
- ``Query``: subject_pattern, view
- ``Grant``: subject, operations, expiry
- ``Revoke``: cap_id
- ``Ok``: data
- ``Event``: event
- ``Error``: message
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import msgpack
import pytest

CONFORMANCE_DIR = Path(__file__).resolve().parents[4] / "docs" / "api" / "conformance" / "wire"

# Field declaration order for each struct-form variant. Source of
# truth: ``crates/ctxd-wire/src/messages.rs``. Keep in sync.
REQUEST_FIELD_ORDER: dict[str, tuple[str, ...]] = {
    "Pub": ("subject", "event_type", "data"),
    "Sub": ("subject_pattern",),
    "Query": ("subject_pattern", "view"),
    "Grant": ("subject", "operations", "expiry"),
    "Revoke": ("cap_id",),
    "PeerHello": ("peer_id", "public_key", "offered_cap", "subjects"),
    "PeerWelcome": ("peer_id", "public_key", "offered_cap", "subjects"),
    "PeerReplicate": ("origin_peer_id", "event"),
    "PeerAck": ("origin_peer_id", "event_id"),
    "PeerCursorRequest": ("peer_id", "subject_pattern"),
    "PeerCursor": ("peer_id", "subject_pattern", "last_event_id", "last_event_time"),
    "PeerFetchEvents": ("event_ids",),
}

RESPONSE_FIELD_ORDER: dict[str, tuple[str, ...]] = {
    "Ok": ("data",),
    "Event": ("event",),
    "Error": ("message",),
}


def _struct_to_positional(
    variant: str,
    inner: dict[str, Any],
    field_order: dict[str, tuple[str, ...]],
) -> list[Any]:
    """Convert a named-field struct dict to a positional array.

    Mirrors ``rmp-serde``'s default struct encoding: emit values in
    declaration order, not key order.
    """
    fields = field_order[variant]
    return [inner[f] for f in fields]


def _normalize_request(value: Any) -> Any:
    """Convert a Request fixture value into the wire-shaped python value.

    - Bare-string nullary variants (e.g. ``"Ping"``) pass through.
    - Map-form struct variants get their inner converted to a
      positional array.
    """
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and len(value) == 1:
        ((variant, inner),) = value.items()
        if isinstance(inner, dict) and variant in REQUEST_FIELD_ORDER:
            return {variant: _struct_to_positional(variant, inner, REQUEST_FIELD_ORDER)}
        return value
    return value


def _normalize_response(value: Any) -> Any:
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and len(value) == 1:
        ((variant, inner),) = value.items()
        if isinstance(inner, dict) and variant in RESPONSE_FIELD_ORDER:
            return {variant: _struct_to_positional(variant, inner, RESPONSE_FIELD_ORDER)}
        return value
    return value


def _wire_pairs() -> list[tuple[str, Path, Path]]:
    """Discover ``(stem, json_path, hex_path)`` triples in the wire corpus."""
    pairs: dict[str, list[Path | None]] = {}
    for path in sorted(CONFORMANCE_DIR.iterdir()):
        name = path.name
        if name.endswith(".msgpack.hex"):
            stem = name[: -len(".msgpack.hex")]
            pairs.setdefault(stem, [None, None])[1] = path
        elif name.endswith(".json"):
            stem = name[: -len(".json")]
            pairs.setdefault(stem, [None, None])[0] = path
    triples: list[tuple[str, Path, Path]] = []
    for stem, (jp, hp) in sorted(pairs.items()):
        if jp is None or hp is None:
            raise AssertionError(f"missing fixture half for {stem}")
        triples.append((stem, jp, hp))
    return triples


@pytest.mark.parametrize(
    "stem,json_path,hex_path",
    _wire_pairs(),
    ids=lambda x: x if isinstance(x, str) else None,
)
def test_wire_msgpack_matches_corpus(stem: str, json_path: Path, hex_path: Path) -> None:
    expected_bytes = bytes.fromhex(hex_path.read_text().strip())
    fixture = json.loads(json_path.read_text())

    # Decide request vs response by file name (matches the Rust
    # conformance harness convention).
    if stem.endswith("_response"):
        wire_value = _normalize_response(fixture)
    else:
        wire_value = _normalize_request(fixture)

    actual = msgpack.packb(wire_value, use_bin_type=True)
    assert actual == expected_bytes, (
        f"wire fixture {stem} msgpack mismatch:\n"
        f"  expected: {expected_bytes.hex()}\n"
        f"  actual:   {actual.hex()}"
    )


def test_wire_corpus_has_minimum_fixtures() -> None:
    """Guard against the corpus shrinking by accident."""
    triples = _wire_pairs()
    assert len(triples) >= 5, f"expected >= 5 wire fixtures, got {len(triples)}"
