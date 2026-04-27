"""Unified error type for the ctxd Python SDK.

The SDK glues HTTP, the wire protocol, and Ed25519 signature
verification into a single :class:`CtxdError` hierarchy so callers can
raise/catch one base class without juggling library-specific exceptions.
"""

from __future__ import annotations


class CtxdError(Exception):
    """Base class for all errors raised by the ctxd SDK."""


class HttpStatusError(CtxdError):
    """HTTP request returned a non-success status with a plain-text body.

    Carries both the status code and the response body so callers retain
    full diagnostic context.
    """

    def __init__(self, status: int, body: str) -> None:
        super().__init__(f"http {status}: {body}")
        self.status = status
        self.body = body


class AuthError(CtxdError):
    """Authorization rejected by the server (HTTP 401 / 403)."""

    def __init__(self, body: str) -> None:
        super().__init__(f"authorization rejected: {body}")
        self.body = body


class NotFoundError(CtxdError):
    """Server returned 404 for the requested resource."""

    def __init__(self, body: str) -> None:
        super().__init__(f"not found: {body}")
        self.body = body


class WireError(CtxdError):
    """Wire-protocol IO or codec failure."""


class WireNotConfiguredError(CtxdError):
    """The SDK was used in a way that requires a wire connection but only HTTP is configured.

    Raised from :meth:`CtxdAsyncClient.write`, ``subscribe``, ``query``,
    ``revoke`` if the caller forgot :meth:`CtxdAsyncClient.with_wire`.
    """

    def __init__(self) -> None:
        super().__init__("wire client not configured: call CtxdAsyncClient.with_wire(addr) first")


class UnexpectedWireResponseError(CtxdError):
    """The server's wire-protocol response did not match what the SDK expected."""

    def __init__(self, message: str) -> None:
        super().__init__(f"unexpected wire response: {message}")
        self.message = message


class SigningError(CtxdError):
    """Ed25519 signature verification input failure (malformed pubkey/signature, etc.).

    The variant carries a short reason string. The SDK does NOT surface
    the underlying cryptography library's error verbatim — those errors
    deliberately don't disclose which check failed (side-channel
    hardening), and we preserve that.
    """


def http_status_error(status: int, body: str) -> CtxdError:
    """Map an HTTP status code to the right :class:`CtxdError` subclass."""
    if status in (401, 403):
        return AuthError(body)
    if status == 404:
        return NotFoundError(body)
    return HttpStatusError(status=status, body=body)
