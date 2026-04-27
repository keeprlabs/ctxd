"""Synchronous wrapper :class:`CtxdClient` around the async client.

The sync API mirrors :class:`CtxdAsyncClient` method-for-method. Each
method runs the corresponding coroutine on a *dedicated* event loop
running in a background thread — that way callers don't fight with an
ambient asyncio loop in their host application, and ``subscribe()`` can
hand back a synchronous iterator that wraps an async iterator running
on the same background loop.

The sync facade is **not** optimal for concurrent workloads. Each call
serializes on the background loop. For high-throughput pipelines, use
:class:`CtxdAsyncClient` directly.
"""

from __future__ import annotations

import asyncio
import threading
from collections.abc import AsyncIterator, Coroutine, Iterator
from concurrent.futures import Future
from datetime import datetime
from typing import Any, TypeVar

from ._client import CtxdAsyncClient
from ._events import Event
from ._http import HealthInfo, PeerInfo, StatsInfo
from ._operation import Operation

T = TypeVar("T")


class _Loop:
    """Long-lived event loop running on a background thread.

    All sync :class:`CtxdClient` calls are dispatched onto this loop
    via :meth:`run`. The loop runs until :meth:`close` is called.
    """

    def __init__(self) -> None:
        self._loop = asyncio.new_event_loop()
        self._ready = threading.Event()
        self._thread = threading.Thread(target=self._run_loop, name="ctxd-sync-loop", daemon=True)
        self._thread.start()
        self._ready.wait()

    def _run_loop(self) -> None:
        asyncio.set_event_loop(self._loop)
        self._ready.set()
        self._loop.run_forever()

    def run(self, coro: Coroutine[Any, Any, T]) -> T:
        """Run ``coro`` on the background loop and return its result."""
        fut: Future[T] = asyncio.run_coroutine_threadsafe(coro, self._loop)
        return fut.result()

    def close(self) -> None:
        """Stop the loop and join the thread."""
        if self._loop.is_closed():
            return
        self._loop.call_soon_threadsafe(self._loop.stop)
        self._thread.join(timeout=5)
        if not self._loop.is_closed():
            # Drain remaining tasks then close.
            try:
                pending = asyncio.all_tasks(loop=self._loop)
                for t in pending:
                    t.cancel()
            except RuntimeError:  # pragma: no cover - loop already stopped
                pass
            self._loop.close()


class CtxdClient:
    """Synchronous facade over :class:`CtxdAsyncClient`.

    Use this in scripts and CLIs where async is unnecessary friction.
    For library and server code, prefer the async client directly.
    """

    def __init__(self, async_client: CtxdAsyncClient, loop: _Loop) -> None:
        self._async = async_client
        self._loop = loop

    @classmethod
    def connect(cls, http_url: str, *, token: str | None = None) -> CtxdClient:
        """Connect to a ctxd daemon's HTTP admin URL."""
        loop = _Loop()
        async_client = CtxdAsyncClient.connect(http_url, token=token)
        return cls(async_client, loop)

    def with_token(self, token: str) -> CtxdClient:
        """Attach a capability token. See :meth:`CtxdAsyncClient.with_token`."""
        self._async.with_token(token)
        return self

    def with_wire(self, wire_addr: str) -> CtxdClient:
        """Connect the wire protocol. See :meth:`CtxdAsyncClient.with_wire`."""
        self._loop.run(self._async.with_wire(wire_addr))
        return self

    @property
    def wire_addr(self) -> str | None:
        """Wire address, if configured."""
        return self._async.wire_addr

    @property
    def http_url(self) -> str:
        """HTTP admin base URL."""
        return self._async.http_url

    def close(self) -> None:
        """Release the HTTP pool, wire connection, and background loop."""
        try:
            self._loop.run(self._async.aclose())
        finally:
            self._loop.close()

    def __enter__(self) -> CtxdClient:
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        self.close()

    def health(self) -> HealthInfo:
        """``GET /health``."""
        return self._loop.run(self._async.health())

    def stats(self) -> StatsInfo:
        """``GET /v1/stats``."""
        return self._loop.run(self._async.stats())

    def grant(
        self,
        subject: str,
        operations: list[Operation] | list[str],
        expires_at: datetime | None = None,
    ) -> str:
        """``POST /v1/grant``."""
        return self._loop.run(self._async.grant(subject, operations, expires_at))

    def peers(self) -> list[PeerInfo]:
        """``GET /v1/peers`` (admin)."""
        return self._loop.run(self._async.peers())

    def peer_remove(self, peer_id: str) -> None:
        """``DELETE /v1/peers/{peer_id}`` (admin)."""
        self._loop.run(self._async.peer_remove(peer_id))

    def write(self, subject: str, event_type: str, data: Any) -> str:
        """Append an event. Returns the new UUIDv7 id."""
        return self._loop.run(self._async.write(subject, event_type, data))

    def query(self, subject_pattern: str, *, view: str = "log") -> list[Event]:
        """Query a materialized view."""
        return self._loop.run(self._async.query(subject_pattern, view=view))

    def revoke(self, token_id: str) -> None:
        """Revoke a capability token (wire ``Revoke`` verb)."""
        self._loop.run(self._async.revoke(token_id))

    def subscribe(self, subject_pattern: str) -> Iterator[Event]:
        """Stream events synchronously.

        Returns a sync iterator that pulls each event off the
        background async loop. Iterating blocks the calling thread
        until the next event arrives or the daemon ends the stream.
        """
        async_iter = self._loop.run(self._async.subscribe(subject_pattern))
        return _SyncIterator(self._loop, async_iter)

    @staticmethod
    def verify_signature(event: Event, pubkey_hex: str) -> bool:
        """Verify an event's Ed25519 signature."""
        return CtxdAsyncClient.verify_signature(event, pubkey_hex)


class _SyncIterator:
    """Sync iterator wrapping an async iterator pinned to a :class:`_Loop`."""

    def __init__(self, loop: _Loop, async_iter: AsyncIterator[Event]) -> None:
        self._loop = loop
        self._async_iter = async_iter

    def __iter__(self) -> _SyncIterator:
        return self

    def __next__(self) -> Event:
        # StopAsyncIteration must NOT cross an `await` boundary as
        # StopIteration — Python rewrites that into RuntimeError. We
        # catch in the sync layer instead.
        async def _step() -> Event | None:
            try:
                event: Event = await self._async_iter.__anext__()
                return event
            except StopAsyncIteration:
                return None

        result = self._loop.run(_step())
        if result is None:
            raise StopIteration
        return result
