"""``/health`` end-to-end test against a real ctxd daemon."""

from __future__ import annotations

from ctxd import CtxdAsyncClient


async def test_health_returns_supported_version(
    ctxd_daemon: tuple[str, str],
) -> None:
    http_url, _ = ctxd_daemon
    async with CtxdAsyncClient.connect(http_url) as client:
        info = await client.health()
        assert info.status == "ok"
        # SDK supports any 0.x daemon — we pin the wire format, not the
        # full minor. Updates to this assertion should track the SDK's
        # actual compatibility window.
        assert info.version.startswith(("0.3.", "0.4.")), (
            f"expected daemon version 0.3.x or 0.4.x, got {info.version}"
        )
