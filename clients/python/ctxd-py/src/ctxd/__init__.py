"""ctxd — Official Python SDK for the ctxd context substrate daemon.

``ctxd`` is the append-only event log + capability layer that AI
agents talk to over MCP, the wire protocol, or HTTP. This package is
the Python SDK every consumer reaches for first: a single
:class:`CtxdAsyncClient` that knows how to mix the HTTP admin surface
(health, grant, peers, stats) with the wire protocol (write,
subscribe, query) — without making you stitch them together.

Quickstart::

    from ctxd import CtxdAsyncClient, Operation

    async with CtxdAsyncClient.connect("http://127.0.0.1:7777") as c:
        await c.with_wire("127.0.0.1:7778")
        eid = await c.write("/work/note", "ctx.note", {"text": "hi"})
        events = await c.query("/work/**", view="log")
        token = await c.grant("/work/**", [Operation.READ])

For scripts and CLIs, the synchronous :class:`CtxdClient` mirrors the
async surface method-for-method::

    from ctxd import CtxdClient
    with CtxdClient.connect("http://127.0.0.1:7777") as c:
        c.with_wire("127.0.0.1:7778")
        c.health()
"""

from __future__ import annotations

from ._client import CtxdAsyncClient
from ._errors import (
    AuthError,
    CtxdError,
    HttpStatusError,
    NotFoundError,
    SigningError,
    UnexpectedWireResponseError,
    WireError,
    WireNotConfiguredError,
)
from ._events import Event, canonical_bytes
from ._http import HealthInfo, PeerInfo, StatsInfo
from ._operation import Operation
from ._signing import verify_signature
from ._sync import CtxdClient

__version__ = "0.4.0"

__all__ = [
    "AuthError",
    "CtxdAsyncClient",
    "CtxdClient",
    "CtxdError",
    "Event",
    "HealthInfo",
    "HttpStatusError",
    "NotFoundError",
    "Operation",
    "PeerInfo",
    "SigningError",
    "StatsInfo",
    "UnexpectedWireResponseError",
    "WireError",
    "WireNotConfiguredError",
    "__version__",
    "canonical_bytes",
    "verify_signature",
]
