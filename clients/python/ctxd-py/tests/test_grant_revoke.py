"""``grant`` / ``revoke`` end-to-end tests."""

from __future__ import annotations

import pytest

from ctxd import CtxdAsyncClient, Operation, UnexpectedWireResponseError


async def test_grant_returns_base64_token(
    ctxd_daemon: tuple[str, str],
) -> None:
    http_url, wire_addr = ctxd_daemon
    async with CtxdAsyncClient.connect(http_url) as client:
        await client.with_wire(wire_addr)
        token = await client.grant(
            "/sdk-test/grant/**",
            [Operation.READ, Operation.SUBJECTS],
        )
        assert token, "token must be non-empty"
        assert len(token) > 32, f"token suspiciously short: {token!r}"
        # Biscuit tokens use base64 (URL-safe alphabet — `[A-Za-z0-9_\-=]`).
        for c in token:
            assert c.isalnum() or c in {"_", "-", "="}, f"non-base64 char in token: {c!r}"


async def test_revoke_via_wire_protocol_returns_not_implemented(
    ctxd_daemon: tuple[str, str],
) -> None:
    """The daemon's REVOKE handler is a stub today; SDK surfaces the error."""
    http_url, wire_addr = ctxd_daemon
    async with CtxdAsyncClient.connect(http_url) as client:
        await client.with_wire(wire_addr)
        with pytest.raises(UnexpectedWireResponseError) as ei:
            await client.revoke("any-id")
        msg = str(ei.value)
        assert "REVOKE" in msg or "not implemented" in msg, (
            f"expected stub-error message, got: {msg}"
        )
