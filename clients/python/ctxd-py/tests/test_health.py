"""``/health`` end-to-end test against a real ctxd daemon."""

from __future__ import annotations

from ctxd import CtxdAsyncClient


async def test_health_returns_v0_3_x_version(
    ctxd_daemon: tuple[str, str],
) -> None:
    http_url, _ = ctxd_daemon
    async with CtxdAsyncClient.connect(http_url) as client:
        info = await client.health()
        assert info.status == "ok"
        assert info.version.startswith("0.3."), (
            f"expected version starting 0.3.x, got {info.version}"
        )
