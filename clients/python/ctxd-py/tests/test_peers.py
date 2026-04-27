"""``/v1/peers`` end-to-end test (admin-required)."""

from __future__ import annotations

import pytest

from ctxd import CtxdAsyncClient, NotFoundError, Operation


async def test_peers_starts_empty_and_remove_404s_on_unknown(
    ctxd_daemon: tuple[str, str],
) -> None:
    http_url, wire_addr = ctxd_daemon
    async with CtxdAsyncClient.connect(http_url) as client:
        await client.with_wire(wire_addr)

        # /v1/peers requires admin; mint one and re-attach.
        admin_token = await client.grant("/", [Operation.ADMIN])

        async with CtxdAsyncClient.connect(http_url, token=admin_token) as admin:
            peers = await admin.peers()
            assert peers == [], f"fresh daemon must have zero peers, got {peers!r}"

            with pytest.raises(NotFoundError):
                await admin.peer_remove("does-not-exist")
