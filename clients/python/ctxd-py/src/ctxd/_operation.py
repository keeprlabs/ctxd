"""Capability operation enum.

Mirrors ``ctxd_cap::Operation`` and the OpenAPI ``operations[]`` enum.
The wire serialization matches the daemon's snake_case names exactly.
"""

from __future__ import annotations

from enum import Enum


class Operation(str, Enum):
    """Operations a capability token can authorize.

    The string values are the wire-format names used in the JSON body of
    ``/v1/grant`` and the wire-protocol ``Grant`` verb. Subclassing
    ``str`` keeps the enum members JSON-serializable without a custom
    encoder.
    """

    READ = "read"
    """Read events under a subject."""

    WRITE = "write"
    """Write (append) events."""

    SUBJECTS = "subjects"
    """List subject paths."""

    SEARCH = "search"
    """FTS / vector search."""

    ADMIN = "admin"
    """Admin operations (mint tokens, manage peers)."""

    def as_wire_str(self) -> str:
        """Wire-format string for this operation."""
        return self.value

    def __str__(self) -> str:  # pragma: no cover - trivial
        return self.value
