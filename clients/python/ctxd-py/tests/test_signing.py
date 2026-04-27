"""Conformance tests for :func:`ctxd.verify_signature`.

Drives :mod:`docs/api/conformance/signatures/*.json` through the
SDK's verifier and asserts each fixture's ``expected`` outcome
matches.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from ctxd import Event, SigningError, verify_signature

CONFORMANCE_DIR = (
    Path(__file__).resolve().parents[4] / "docs" / "api" / "conformance" / "signatures"
)


def _signature_fixtures() -> list[Path]:
    fixtures = sorted(p for p in CONFORMANCE_DIR.iterdir() if p.suffix == ".json")
    if len(fixtures) < 3:
        raise AssertionError(
            f"expected at least 3 signature fixtures in {CONFORMANCE_DIR}, found {len(fixtures)}"
        )
    return fixtures


@pytest.mark.parametrize("fixture_path", _signature_fixtures(), ids=lambda p: p.stem)
def test_signature_corpus_matches_expected(fixture_path: Path) -> None:
    j = json.loads(fixture_path.read_text())
    event = Event.model_validate(j["event"])
    event.signature = j["signature"]
    pubkey_hex = j["public_key_hex"]
    expected = bool(j["expected"])
    actual = verify_signature(event, pubkey_hex)
    assert actual is expected, (
        f"signature fixture {fixture_path.name}: expected {expected}, got {actual}"
    )


def test_unsigned_event_returns_false() -> None:
    event = Event(
        id="01900000-0000-7000-8000-000000000099",
        source="ctxd://test",
        subject="/t/u",
        type="demo",
        time="2026-01-01T00:00:00Z",  # type: ignore[arg-type]
        data={},
    )
    # Any pubkey — unsigned events always return False.
    pubkey = "00" * 32
    assert verify_signature(event, pubkey) is False


def test_malformed_pubkey_hex_raises() -> None:
    event = Event(
        id="01900000-0000-7000-8000-000000000099",
        source="ctxd://test",
        subject="/t/u",
        type="demo",
        time="2026-01-01T00:00:00Z",  # type: ignore[arg-type]
        data={},
    )
    with pytest.raises(SigningError):
        verify_signature(event, "not-hex!!")


def test_wrong_length_pubkey_raises() -> None:
    event = Event(
        id="01900000-0000-7000-8000-000000000099",
        source="ctxd://test",
        subject="/t/u",
        type="demo",
        time="2026-01-01T00:00:00Z",  # type: ignore[arg-type]
        data={},
    )
    short = "ab" * 30
    with pytest.raises(SigningError):
        verify_signature(event, short)
