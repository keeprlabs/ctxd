"""Async wire-protocol client.

Low-level framing + msgpack encoding matching ``rmp-serde``'s
externally-tagged enum encoding. The shape of every message is pinned
by ``docs/api/conformance/wire/*.{json,msgpack.hex}`` — those fixtures
are the test oracle.

Framing:

- 4-byte big-endian length prefix.
- Body is a single MessagePack value.

Enum encoding (rmp-serde default):

- ``Request::Ping`` (nullary) -> bare string ``"Ping"``.
- ``Request::Pub { subject, event_type, data }`` -> map of one entry
  ``{"Pub": [subject, event_type, data]}``. The inner is a *positional
  array* in field declaration order, NOT a map. Same for every
  struct-form variant.
- ``Response::Ok { data }`` -> ``{"Ok": [data]}``.
- ``Response::Pong`` -> ``"Pong"``.

The :class:`WireConn` here is a single TCP connection. Subscriptions
take exclusive ownership of a connection (the daemon puts the socket
into streaming-receive mode after a ``Sub``); :meth:`subscribe` opens a
fresh TCP connection per call.
"""

from __future__ import annotations

import asyncio
import logging
from collections.abc import AsyncIterator
from typing import Any

import msgpack

from ._errors import (
    UnexpectedWireResponseError,
    WireError,
)
from ._events import Event

_LOG = logging.getLogger("ctxd")

MAX_FRAME_BYTES = 16 * 1024 * 1024
"""16 MiB ceiling. A reader that observes a length prefix larger than
this MUST reject the frame *before* allocating the buffer.

Same constant as the Rust wire crate's ``MAX_FRAME_BYTES``."""


def encode_request(req: dict[str, Any] | str) -> bytes:
    """Encode a Request to msgpack bytes (no framing).

    Accepts either a one-key map ``{"VariantName": <inner>}`` (struct
    variant) or the bare string variant name (nullary variant). The
    caller is responsible for shaping ``inner`` as a positional array
    (rmp-serde's struct encoding).
    """
    out: bytes = msgpack.packb(req, use_bin_type=True)
    return out


def decode_response(payload: bytes) -> Any:
    """Decode a single msgpack response body. ``raw=False`` to get strings."""
    return msgpack.unpackb(payload, raw=False)


class WireConn:
    """A single async TCP connection to the daemon's wire-protocol port.

    Use :meth:`connect` to construct, :meth:`close` (or async
    ``CtxdAsyncClient.aclose``) to release the connection. Methods
    that send a request and read a response are mutually exclusive —
    do not interleave from multiple tasks against the same
    :class:`WireConn`.
    """

    def __init__(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
        addr: str,
    ) -> None:
        self._reader = reader
        self._writer = writer
        self._addr = addr
        self._lock = asyncio.Lock()

    @classmethod
    async def connect(cls, addr: str) -> WireConn:
        """Open a fresh TCP connection to ``addr`` (``host:port``)."""
        host, _, port_str = addr.rpartition(":")
        if not host or not port_str:
            raise WireError(f"invalid wire address: {addr!r}")
        try:
            port = int(port_str)
        except ValueError as e:
            raise WireError(f"invalid wire port: {port_str!r}") from e
        try:
            reader, writer = await asyncio.open_connection(host, port)
        except OSError as e:
            raise WireError(f"failed to connect wire {addr}: {e}") from e
        return cls(reader, writer, addr)

    @property
    def addr(self) -> str:
        """The address this connection was opened against."""
        return self._addr

    async def close(self) -> None:
        """Close the underlying TCP connection."""
        try:
            self._writer.close()
            await self._writer.wait_closed()
        except Exception:  # pragma: no cover - best-effort
            pass

    async def _write_frame(self, payload: bytes) -> None:
        if len(payload) > MAX_FRAME_BYTES:
            raise WireError(f"frame too large to send: {len(payload)} bytes")
        header = len(payload).to_bytes(4, "big", signed=False)
        self._writer.write(header)
        self._writer.write(payload)
        await self._writer.drain()

    async def _read_frame(self) -> bytes | None:
        """Read one frame; return ``None`` on a clean EOF at frame boundary."""
        try:
            header = await self._reader.readexactly(4)
        except asyncio.IncompleteReadError as e:
            if not e.partial:
                # Clean close at frame boundary.
                return None
            raise WireError("truncated frame header") from e
        length = int.from_bytes(header, "big", signed=False)
        if length > MAX_FRAME_BYTES:
            raise WireError(f"frame too large: {length} bytes")
        try:
            body = await self._reader.readexactly(length)
        except asyncio.IncompleteReadError as e:
            raise WireError("truncated frame body") from e
        return body

    async def _request(self, req: dict[str, Any] | str) -> Any:
        """Send one request, read one response. Holds an internal lock."""
        async with self._lock:
            await self._write_frame(encode_request(req))
            body = await self._read_frame()
            if body is None:
                raise WireError("connection closed before response")
            return decode_response(body)

    async def ping(self) -> None:
        """Send a ``Ping`` and assert the daemon responds ``Pong``."""
        resp = await self._request("Ping")
        if resp != "Pong":
            raise UnexpectedWireResponseError(f"expected Pong, got {resp!r}")

    async def publish(self, subject: str, event_type: str, data: Any) -> str:
        """Send ``Pub`` and return the new event's id (UUIDv7 string)."""
        resp = await self._request({"Pub": [subject, event_type, data]})
        ok = _expect_ok(resp)
        if not isinstance(ok, dict) or not isinstance(ok.get("id"), str):
            raise UnexpectedWireResponseError(f"Pub response missing string `id` field: {ok!r}")
        eid: str = ok["id"]
        return eid

    async def query(self, subject_pattern: str, view: str) -> list[Event]:
        """Send ``Query`` and return the parsed events.

        The KV view returns a single value, not a list; this method
        raises :class:`UnexpectedWireResponseError` for that case so
        callers don't silently get the wrong shape. Drop down to the
        raw wire request to handle the KV view.
        """
        resp = await self._request({"Query": [subject_pattern, view]})
        ok = _expect_ok(resp)
        if view == "kv":
            raise UnexpectedWireResponseError(
                "kv view returns a value, not a list of events; use the wire APIs directly"
            )
        if not isinstance(ok, list):
            raise UnexpectedWireResponseError(f"Query response data is not a list: {ok!r}")
        return [Event.model_validate(e) for e in ok]

    async def revoke(self, cap_id: str) -> None:
        """Send ``Revoke``; raise :class:`UnexpectedWireResponseError` on Error."""
        resp = await self._request({"Revoke": [cap_id]})
        _expect_ok(resp)

    async def subscribe(self, subject_pattern: str) -> AsyncIterator[Event]:
        """Open a *fresh* connection and stream events for ``subject_pattern``.

        Returns an async iterator; iterate with ``async for`` until the
        daemon ends the stream or the caller breaks out.
        """
        sub_conn = await WireConn.connect(self._addr)
        return _subscription_iter(sub_conn, subject_pattern)


async def _subscription_iter(conn: WireConn, subject_pattern: str) -> AsyncIterator[Event]:
    """Drive a subscription connection: send ``Sub``, then yield Events."""
    try:
        async with conn._lock:
            await conn._write_frame(encode_request({"Sub": [subject_pattern]}))
            while True:
                body = await conn._read_frame()
                if body is None:
                    return
                resp = decode_response(body)
                if isinstance(resp, dict) and "Event" in resp:
                    inner = resp["Event"]
                    # Inner is the positional array `[event]`.
                    if isinstance(inner, list) and len(inner) == 1:
                        yield Event.model_validate(inner[0])
                    elif isinstance(inner, dict):
                        # Be lenient: some encoders prefer maps.
                        yield Event.model_validate(inner.get("event", inner))
                    else:
                        raise UnexpectedWireResponseError(f"unexpected Event shape: {resp!r}")
                elif resp == "EndOfStream":
                    return
                elif isinstance(resp, dict) and "Error" in resp:
                    inner = resp["Error"]
                    if isinstance(inner, list) and len(inner) == 1:
                        raise UnexpectedWireResponseError(str(inner[0]))
                    raise UnexpectedWireResponseError(str(inner))
                else:
                    raise UnexpectedWireResponseError(f"expected Event/EndOfStream, got {resp!r}")
    finally:
        await conn.close()


def _expect_ok(resp: Any) -> Any:
    """Unwrap a ``Response::Ok``'s data, or raise on non-Ok variants.

    rmp-serde encodes ``Response::Ok { data }`` as ``{"Ok": [data]}``.
    We tolerate ``{"Ok": {"data": ...}}`` for codecs that produce
    map-form structs.
    """
    if resp == "Pong":
        return None
    if isinstance(resp, dict) and "Ok" in resp:
        inner = resp["Ok"]
        if isinstance(inner, list) and len(inner) == 1:
            return inner[0]
        if isinstance(inner, dict):
            return inner.get("data", inner)
        return inner
    if isinstance(resp, dict) and "Error" in resp:
        inner = resp["Error"]
        if isinstance(inner, list) and len(inner) == 1:
            raise UnexpectedWireResponseError(str(inner[0]))
        if isinstance(inner, dict):
            raise UnexpectedWireResponseError(str(inner.get("message", inner)))
        raise UnexpectedWireResponseError(str(inner))
    raise UnexpectedWireResponseError(f"expected Ok, got {resp!r}")
