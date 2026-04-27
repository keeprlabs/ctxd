"""``/v1/stats`` end-to-end test."""

from __future__ import annotations

from ctxd import CtxdAsyncClient


async def test_stats_returns_subject_count(
    ctxd_daemon: tuple[str, str],
) -> None:
    http_url, wire_addr = ctxd_daemon
    async with CtxdAsyncClient.connect(http_url) as client:
        await client.with_wire(wire_addr)
        await client.write("/sdk-test/stats/one", "ctx.note", {"k": "v"})
        stats = await client.stats()
        assert stats.subject_count >= 1, (
            f"subject_count must be >= 1 after a write, got {stats.subject_count}"
        )
