"""End-to-end tests covering ``write``, ``query``, and ``subscribe``."""

from __future__ import annotations

import asyncio

import pytest

from ctxd import CtxdAsyncClient, WireNotConfiguredError


async def test_write_then_query_log_returns_event(
    ctxd_daemon: tuple[str, str],
) -> None:
    http_url, wire_addr = ctxd_daemon
    async with CtxdAsyncClient.connect(http_url) as client:
        await client.with_wire(wire_addr)

        eid = await client.write("/sdk-test/log/one", "ctx.note", {"content": "hello sdk"})

        events = await client.query("/sdk-test/log/one", view="log")
        assert any(e.id == eid for e in events), (
            f"queried log did not contain just-written event id {eid}"
        )


async def test_subscribe_yields_published_event(
    ctxd_daemon: tuple[str, str],
) -> None:
    http_url, wire_addr = ctxd_daemon
    async with CtxdAsyncClient.connect(http_url) as client:
        await client.with_wire(wire_addr)

        # Open the subscription FIRST so the daemon registers the
        # broadcast receiver before we publish. Then schedule a
        # delayed publish on a separate connection. We cap the read
        # at 5s so a daemon hang fails the test rather than wedging
        # the runner.
        stream = await client.subscribe("/sdk-test/sub/**")

        async def _delayed_publish() -> str:
            # 50ms is generous on every CI we've seen — the broadcast
            # registration is synchronous before the daemon ack, but
            # we leave the race-margin in for slow runners.
            await asyncio.sleep(0.05)
            async with CtxdAsyncClient.connect(http_url) as pub:
                await pub.with_wire(wire_addr)
                return await pub.write("/sdk-test/sub/event", "ctx.note", {"msg": "via sub"})

        pub_task = asyncio.create_task(_delayed_publish())

        try:
            event = await asyncio.wait_for(stream.__anext__(), timeout=5.0)
        except StopAsyncIteration as e:
            raise AssertionError("subscription ended before event arrived") from e

        written_id = await pub_task
        assert event.id == written_id, "subscribed event id != written id"
        assert event.event_type == "ctx.note"


async def test_write_without_wire_raises(
    ctxd_daemon: tuple[str, str],
) -> None:
    http_url, _ = ctxd_daemon
    async with CtxdAsyncClient.connect(http_url) as client:
        with pytest.raises(WireNotConfiguredError):
            await client.write("/x", "demo", {})
