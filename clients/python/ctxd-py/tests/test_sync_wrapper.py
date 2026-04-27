"""Smoke tests for the synchronous :class:`CtxdClient` facade.

We exercise just enough of the sync API to confirm the background
loop starts, requests dispatch, and ``close()`` shuts everything down.
The async client owns the deep test coverage; the sync wrapper is a
thin forwarder.

These tests run a real daemon via the same ``ctxd_daemon`` fixture as
the async suite.
"""

from __future__ import annotations

from ctxd import CtxdClient, Operation


def test_sync_health_and_write_and_query(
    ctxd_daemon: tuple[str, str],
) -> None:
    http_url, wire_addr = ctxd_daemon
    with CtxdClient.connect(http_url) as client:
        client.with_wire(wire_addr)
        info = client.health()
        assert info.status == "ok"

        eid = client.write("/sdk-test/sync/one", "ctx.note", {"content": "sync hi"})
        events = client.query("/sdk-test/sync/one", view="log")
        assert any(e.id == eid for e in events)

        # Mint a token and verify it round-trips as a string.
        token = client.grant("/sdk-test/sync/**", [Operation.READ])
        assert token and len(token) > 32

        stats = client.stats()
        assert stats.subject_count >= 1
