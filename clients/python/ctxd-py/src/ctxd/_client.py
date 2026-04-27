"""High-level async :class:`CtxdAsyncClient` facade.

Wraps the lower-level :class:`HttpAdminClient` and the
lazy-instantiated wire connection so a typical "connect -> write ->
query" flow looks the same as in any modern async SDK::

    async with CtxdAsyncClient.connect("http://127.0.0.1:7777") as client:
        await client.with_wire("127.0.0.1:7778")
        eid = await client.write("/work/note", "ctx.note", {"text": "hi"})
        events = await client.query("/work/**", view="log")
"""

from __future__ import annotations

import logging
from collections.abc import AsyncIterator
from datetime import datetime
from types import TracebackType
from typing import Any

from ._errors import WireNotConfiguredError
from ._events import Event
from ._http import HealthInfo, HttpAdminClient, PeerInfo, StatsInfo
from ._operation import Operation
from ._signing import verify_signature as _verify_signature_fn
from ._wire import WireConn

_LOG = logging.getLogger("ctxd")


class CtxdAsyncClient:
    """High-level async ctxd client.

    Holds an HTTP admin client (always present) and an optional wire
    connection (lazy — opened by :meth:`with_wire`). Use as an async
    context manager to release both on exit::

        async with CtxdAsyncClient.connect(url) as c:
            ...
    """

    def __init__(self, http: HttpAdminClient) -> None:
        self._http = http
        self._wire: WireConn | None = None
        self._wire_addr: str | None = None

    @classmethod
    def connect(cls, http_url: str, *, token: str | None = None) -> CtxdAsyncClient:
        """Connect to a ctxd daemon's HTTP admin URL.

        The URL must include scheme + host + port (e.g.
        ``http://127.0.0.1:7777``). This call only constructs the
        underlying :class:`httpx.AsyncClient` — it does *not* issue a
        network request. Use :meth:`health` to verify the daemon is
        reachable.

        Synchronous on purpose: the Rust SDK's analogue is async only
        because Rust has no notion of "construct without IO". In
        Python, ``httpx.AsyncClient`` constructs without IO too — we
        match the user-facing ergonomic by dropping the ``await``::

            async with CtxdAsyncClient.connect(url) as client:
                await client.with_wire(wire_addr)
                ...
        """
        http = HttpAdminClient(http_url, token=token)
        return cls(http)

    def with_token(self, token: str) -> CtxdAsyncClient:
        """Attach a capability token to all admin calls.

        Sent as ``Authorization: Bearer <token>`` on every HTTP
        request. Replaces any previously attached token.
        """
        self._http.with_token(token)
        return self

    async def with_wire(self, wire_addr: str) -> CtxdAsyncClient:
        """Connect the wire protocol (TCP + msgpack) at ``wire_addr``.

        Required for :meth:`write`, :meth:`subscribe`, :meth:`query`,
        and :meth:`revoke`. Replaces any previously-open wire
        connection.
        """
        if self._wire is not None:
            await self._wire.close()
        self._wire = await WireConn.connect(wire_addr)
        self._wire_addr = wire_addr
        return self

    @property
    def wire_addr(self) -> str | None:
        """Wire address this client was configured with, if any."""
        return self._wire_addr

    @property
    def http_url(self) -> str:
        """HTTP admin base URL."""
        return self._http.base_url

    async def aclose(self) -> None:
        """Release the HTTP pool and the wire connection (if any)."""
        if self._wire is not None:
            await self._wire.close()
            self._wire = None
        await self._http.aclose()

    async def __aenter__(self) -> CtxdAsyncClient:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        await self.aclose()

    # ----- HTTP admin endpoints -----

    async def health(self) -> HealthInfo:
        """``GET /health`` — daemon liveness + version probe."""
        return await self._http.health()

    async def stats(self) -> StatsInfo:
        """``GET /v1/stats`` — basic store statistics."""
        return await self._http.stats()

    async def grant(
        self,
        subject: str,
        operations: list[Operation] | list[str],
        expires_at: datetime | None = None,
    ) -> str:
        """``POST /v1/grant`` — mint a capability token."""
        return await self._http.grant(subject, operations, expires_at)

    async def peers(self) -> list[PeerInfo]:
        """``GET /v1/peers`` — list federation peers (admin)."""
        return await self._http.peers()

    async def peer_remove(self, peer_id: str) -> None:
        """``DELETE /v1/peers/{peer_id}`` — remove a federation peer."""
        await self._http.peer_remove(peer_id)

    # ----- Wire-protocol verbs -----

    def _require_wire(self) -> WireConn:
        if self._wire is None:
            raise WireNotConfiguredError()
        return self._wire

    async def write(self, subject: str, event_type: str, data: Any) -> str:
        """Append an event under ``subject``. Returns the new UUIDv7 id."""
        wire = self._require_wire()
        return await wire.publish(subject, event_type, data)

    async def query(self, subject_pattern: str, *, view: str = "log") -> list[Event]:
        """Query a materialized view. Returns the event list for ``log`` / ``fts`` views."""
        wire = self._require_wire()
        return await wire.query(subject_pattern, view)

    async def subscribe(self, subject_pattern: str) -> AsyncIterator[Event]:
        """Subscribe to events matching ``subject_pattern``.

        Opens a *fresh* TCP connection (a subscription puts a
        connection into streaming-receive mode and can't be reused for
        further requests). Iterate with ``async for``::

            async for event in client.subscribe("/work/**"):
                ...
        """
        wire = self._require_wire()
        return await wire.subscribe(subject_pattern)

    async def revoke(self, token_id: str) -> None:
        """Revoke a capability token by id (wire ``Revoke`` verb)."""
        wire = self._require_wire()
        await wire.revoke(token_id)

    # ----- Pure helpers -----

    @staticmethod
    def verify_signature(event: Event, pubkey_hex: str) -> bool:
        """Verify an event's Ed25519 signature against a hex-encoded pubkey."""
        return _verify_signature_fn(event, pubkey_hex)
