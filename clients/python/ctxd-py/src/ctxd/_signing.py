"""Ed25519 signature verification.

Re-implements the daemon-side canonical-bytes routine from
``ctxd_core::signing`` so SDK consumers can verify an event's signature
without reaching for an extra library. Pinned to the daemon side via
the ``docs/api/conformance/signatures/*.json`` fixtures — if either
implementation drifts, the conformance test breaks.
"""

from __future__ import annotations

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

from ._errors import SigningError
from ._events import Event, canonical_bytes


def verify_signature(event: Event, pubkey_hex: str) -> bool:
    """Verify an event's Ed25519 signature against a hex-encoded public key.

    ``pubkey_hex`` is a 64-character hex string (32 bytes). The
    event's own :attr:`Event.signature` field is read; if it is
    ``None``, this returns ``False`` rather than raising — an unsigned
    event is indistinguishable from a tampered one for callers asking
    "is this signed by ``pubkey_hex``?".

    Raises :class:`SigningError` only for hard input failures: malformed
    hex or wrong-length pubkey.
    """
    pubkey_bytes = _decode_pubkey(pubkey_hex)

    sig_hex = event.signature
    if sig_hex is None:
        # Unsigned events: "is this signed by this key?" -> No.
        return False

    try:
        sig_bytes = bytes.fromhex(sig_hex.strip())
    except ValueError as e:
        raise SigningError(f"invalid signature hex: {e}") from None
    if len(sig_bytes) != 64:
        # Wrong-length signature -> not this signature. Match the Rust
        # SDK's behavior of returning False rather than raising on
        # this edge.
        return False

    try:
        verifying_key = Ed25519PublicKey.from_public_bytes(pubkey_bytes)
    except Exception as e:  # pragma: no cover - cryptography rejects up-front
        raise SigningError(f"invalid pubkey: {e}") from None

    canonical = canonical_bytes(event)

    try:
        verifying_key.verify(sig_bytes, canonical)
    except InvalidSignature:
        return False
    return True


def _decode_pubkey(pubkey_hex: str) -> bytes:
    """Decode a 32-byte Ed25519 public key from hex. Raises :class:`SigningError`."""
    try:
        pubkey_bytes = bytes.fromhex(pubkey_hex.strip())
    except ValueError as e:
        raise SigningError(f"invalid pubkey hex: {e}") from None
    if len(pubkey_bytes) != 32:
        raise SigningError("pubkey must be 32 bytes (64 hex chars)")
    return pubkey_bytes
