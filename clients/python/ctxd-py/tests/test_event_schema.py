"""Event-schema conformance.

For each fixture in ``docs/api/conformance/events/*.json``, parse it
into an :class:`Event` and re-serialize via :meth:`Event.model_dump_wire`.
Assert the structural JSON shape is identical.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from ctxd import Event, verify_signature

CONFORMANCE_DIR = Path(__file__).resolve().parents[4] / "docs" / "api" / "conformance" / "events"


def _event_fixtures() -> list[Path]:
    fixtures = sorted(p for p in CONFORMANCE_DIR.iterdir() if p.suffix == ".json")
    if len(fixtures) < 3:
        raise AssertionError(
            f"expected at least 3 event fixtures in {CONFORMANCE_DIR}, found {len(fixtures)}"
        )
    return fixtures


@pytest.mark.parametrize("fixture_path", _event_fixtures(), ids=lambda p: p.stem)
def test_event_roundtrips_structurally(fixture_path: Path) -> None:
    raw = json.loads(fixture_path.read_text())
    event = Event.model_validate(raw)
    re = event.model_dump_wire()
    assert re == raw, (
        f"event fixture {fixture_path.name} did not roundtrip:\n"
        f"  original: {json.dumps(raw, indent=2, sort_keys=True)}\n"
        f"  reserialized: {json.dumps(re, indent=2, sort_keys=True)}"
    )


def test_signed_event_in_corpus_verifies() -> None:
    """The shipped ``signed.json`` must verify against ``signed.pubkey.hex``."""
    signed_path = CONFORMANCE_DIR / "signed.json"
    pubkey_path = CONFORMANCE_DIR / "signed.pubkey.hex"
    event = Event.model_validate(json.loads(signed_path.read_text()))
    pubkey_hex = pubkey_path.read_text().strip()
    assert verify_signature(event, pubkey_hex) is True
