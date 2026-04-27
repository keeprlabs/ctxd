"""HTTP admin client for the ctxd daemon.

Hand-written types matching ``docs/api/openapi.yaml``. We deliberately
avoid OpenAPI codegen for v0.3 — the surface is small, the shapes are
stable, and a hand-rolled client is easier to read in an incident.

Auth model: every constructor accepts an optional bearer token. When
set, it is attached as ``Authorization: Bearer <token>`` on every
request. Endpoints documented as open (``/health``, ``/v1/grant``,
``/v1/stats``, ``/v1/approvals``) tolerate the header anyway, so we
send it unconditionally when configured.
"""

from __future__ import annotations

import logging
from datetime import datetime, timezone
from typing import Any

import httpx
from pydantic import BaseModel

from ._errors import http_status_error
from ._operation import Operation

DEFAULT_TIMEOUT = 10.0
"""Per-call HTTP timeout (seconds). Long enough to forgive a slow loopback
under contention but short enough that a misconfigured URL fails loudly
rather than hanging."""

_LOG = logging.getLogger("ctxd")


class HealthInfo(BaseModel):
    """``GET /health`` response body."""

    status: str
    version: str


class StatsInfo(BaseModel):
    """``GET /v1/stats`` response body. Future versions may add fields."""

    subject_count: int

    model_config = {"extra": "ignore"}


class PeerInfo(BaseModel):
    """``GET /v1/peers`` response item."""

    peer_id: str
    url: str
    public_key: str
    subject_patterns: list[str]
    added_at: str
    last_seen_at: str | None = None


class HttpAdminClient:
    """Async HTTP admin client.

    Holds an :class:`httpx.AsyncClient`. Use :meth:`aclose` (or the
    async context manager on :class:`CtxdAsyncClient`) to release the
    underlying connection pool.
    """

    def __init__(self, base_url: str, *, token: str | None = None) -> None:
        self._base = base_url.rstrip("/")
        self._token = token
        self._client = httpx.AsyncClient(timeout=DEFAULT_TIMEOUT)

    @property
    def base_url(self) -> str:
        """Base URL the client is pointed at (with any trailing slash stripped)."""
        return self._base

    @property
    def token(self) -> str | None:
        """Bearer token attached to every request, or ``None``."""
        return self._token

    def with_token(self, token: str) -> HttpAdminClient:
        """Return ``self`` after attaching a capability token.

        Mutates in place to keep the API ergonomic for chained calls.
        Replaces any previously attached token.
        """
        self._token = token
        return self

    async def aclose(self) -> None:
        """Close the underlying HTTP connection pool."""
        await self._client.aclose()

    def _headers(self) -> dict[str, str]:
        """Build the per-request headers, including Bearer auth if set.

        We never log the token here — every log emits in this module
        treats the token as opaque secret data.
        """
        if self._token:
            return {"Authorization": f"Bearer {self._token}"}
        return {}

    async def _get_json(self, path: str) -> Any:
        url = f"{self._base}{path}"
        _LOG.debug("ctxd http GET %s", url)
        resp = await self._client.get(url, headers=self._headers())
        return _decode_json(resp)

    async def _post_json(self, path: str, body: dict[str, Any]) -> Any:
        url = f"{self._base}{path}"
        _LOG.debug("ctxd http POST %s", url)
        resp = await self._client.post(url, json=body, headers=self._headers())
        return _decode_json(resp)

    async def _delete(self, path: str) -> None:
        url = f"{self._base}{path}"
        _LOG.debug("ctxd http DELETE %s", url)
        resp = await self._client.delete(url, headers=self._headers())
        if not (200 <= resp.status_code < 300):
            raise http_status_error(resp.status_code, resp.text)

    async def health(self) -> HealthInfo:
        """``GET /health`` — daemon liveness + version probe."""
        return HealthInfo.model_validate(await self._get_json("/health"))

    async def stats(self) -> StatsInfo:
        """``GET /v1/stats`` — basic store statistics."""
        return StatsInfo.model_validate(await self._get_json("/v1/stats"))

    async def grant(
        self,
        subject: str,
        operations: list[Operation] | list[str],
        expires_at: datetime | None = None,
    ) -> str:
        """``POST /v1/grant`` — mint a capability token.

        Returns the base64-encoded token (the ``token`` field of the
        daemon's response).
        """
        ops: list[str] = [o.value if isinstance(o, Operation) else str(o) for o in operations]
        body: dict[str, Any] = {"subject": subject, "operations": ops}
        if expires_at is not None:
            now = datetime.now(timezone.utc)
            if expires_at.tzinfo is None:
                expires_at = expires_at.replace(tzinfo=timezone.utc)
            delta_s = int((expires_at - now).total_seconds())
            # The daemon requires a positive integer; clamp at 1 so a
            # caller passing a past timestamp surfaces a clear 400 from
            # the server rather than a confusing "expires_in_secs: 0"
            # rejection.
            body["expires_in_secs"] = max(1, delta_s)
        resp = await self._post_json("/v1/grant", body)
        token = resp.get("token") if isinstance(resp, dict) else None
        if not isinstance(token, str):
            from ._errors import UnexpectedWireResponseError

            raise UnexpectedWireResponseError(
                f"grant response missing string `token` field: {resp!r}"
            )
        return token

    async def peers(self) -> list[PeerInfo]:
        """``GET /v1/peers`` — list federation peers (admin)."""
        resp = await self._get_json("/v1/peers")
        peers_raw = resp.get("peers", []) if isinstance(resp, dict) else []
        return [PeerInfo.model_validate(p) for p in peers_raw]

    async def peer_remove(self, peer_id: str) -> None:
        """``DELETE /v1/peers/{peer_id}`` — remove a federation peer (admin)."""
        await self._delete(f"/v1/peers/{peer_id}")


def _decode_json(resp: httpx.Response) -> Any:
    """Decode ``resp`` as JSON or raise the right :class:`CtxdError`."""
    if 200 <= resp.status_code < 300:
        return resp.json()
    raise http_status_error(resp.status_code, resp.text)
