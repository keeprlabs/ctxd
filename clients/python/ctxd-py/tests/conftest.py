"""Pytest fixtures: a real ``ctxd serve`` daemon per test.

The :func:`ctxd_daemon` fixture builds the workspace ``ctxd`` binary
on first use, picks two free ports, spawns the daemon with a tempdir
DB, and waits for both ``/health`` and the wire-protocol port to
accept connections before yielding ``(http_url, wire_addr)``.

We deliberately spawn one daemon per test for isolation. Tests in this
suite are quick (sub-second after warm-up); the cost of a fresh
process is small compared to the surface a clean DB protects.
"""

from __future__ import annotations

import asyncio
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from collections.abc import AsyncIterator
from contextlib import closing
from pathlib import Path

import httpx
import pytest


def _workspace_root() -> Path:
    """Walk up from this test file until we find ``Cargo.lock``."""
    cursor = Path(__file__).resolve().parent
    for _ in range(10):
        if (cursor / "Cargo.lock").exists():
            return cursor
        cursor = cursor.parent
    raise RuntimeError("Cargo.lock not found walking up from conftest.py")


WORKSPACE_ROOT = _workspace_root()
CTXD_DEBUG = WORKSPACE_ROOT / "target" / "debug" / "ctxd"


def _ensure_ctxd_built() -> Path:
    """Build the workspace ``ctxd`` binary if missing. Returns its path."""
    if CTXD_DEBUG.exists():
        return CTXD_DEBUG
    cargo = shutil.which("cargo")
    if cargo is None:
        raise RuntimeError("cargo not found on PATH; install Rust to run integration tests")
    build = subprocess.run(  # noqa: S603 - controlled cargo call
        [cargo, "build", "--quiet", "--bin", "ctxd"],
        cwd=WORKSPACE_ROOT,
        check=False,
    )
    if build.returncode != 0:
        raise RuntimeError(f"cargo build --bin ctxd exited {build.returncode}")
    if not CTXD_DEBUG.exists():
        raise RuntimeError(f"cargo build succeeded but {CTXD_DEBUG} is missing")
    return CTXD_DEBUG


def _pick_free_port() -> int:
    """Pick a free TCP port on 127.0.0.1.

    There's a small TOCTOU window between this call and the daemon
    binding, but tests aren't fighting over loopback ports in
    practice.
    """
    with closing(socket.socket(socket.AF_INET, socket.SOCK_STREAM)) as s:
        s.bind(("127.0.0.1", 0))
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        return s.getsockname()[1]


async def _wait_for_health(http_url: str, deadline_s: float = 30.0) -> None:
    """Poll ``/health`` until it answers 200 or the deadline elapses."""
    started = time.monotonic()
    last_err: BaseException | None = None
    async with httpx.AsyncClient(timeout=0.5) as client:
        while time.monotonic() - started < deadline_s:
            try:
                resp = await client.get(f"{http_url}/health")
                if resp.status_code == 200:
                    return
                last_err = AssertionError(f"status {resp.status_code}")
            except (httpx.HTTPError, OSError) as e:
                last_err = e
            await asyncio.sleep(0.1)
    raise AssertionError(
        f"daemon /health did not become ready within {deadline_s}s; last error: {last_err!r}"
    )


async def _wait_for_wire(wire_addr: str, deadline_s: float = 30.0) -> None:
    """Poll the wire port until a TCP connect succeeds."""
    host, _, port_str = wire_addr.rpartition(":")
    port = int(port_str)
    started = time.monotonic()
    last_err: BaseException | None = None
    while time.monotonic() - started < deadline_s:
        try:
            reader, writer = await asyncio.open_connection(host, port)
            writer.close()
            try:
                await writer.wait_closed()
            except Exception:  # pragma: no cover - best-effort close
                pass
            del reader  # silence linters
            return
        except OSError as e:
            last_err = e
        await asyncio.sleep(0.1)
    raise AssertionError(
        f"wire port {wire_addr} never accepted connections; last error: {last_err!r}"
    )


@pytest.fixture(scope="session")
def ctxd_binary() -> Path:
    """Build the ctxd binary once per pytest session."""
    return _ensure_ctxd_built()


@pytest.fixture
async def ctxd_daemon(ctxd_binary: Path) -> AsyncIterator[tuple[str, str]]:
    """Spawn a fresh ``ctxd serve`` and yield ``(http_url, wire_addr)``."""
    http_port = _pick_free_port()
    wire_port = _pick_free_port()
    while wire_port == http_port:  # pragma: no cover - extremely rare
        wire_port = _pick_free_port()

    http_addr = f"127.0.0.1:{http_port}"
    wire_addr = f"127.0.0.1:{wire_port}"
    http_url = f"http://{http_addr}"

    tempdir = tempfile.mkdtemp(prefix="ctxd-py-test-")
    db_path = os.path.join(tempdir, "ctxd.db")

    proc = subprocess.Popen(  # noqa: S603 - controlled binary
        [
            str(ctxd_binary),
            "--db",
            db_path,
            "serve",
            "--bind",
            http_addr,
            "--wire-bind",
            wire_addr,
        ],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    try:
        await _wait_for_health(http_url)
        await _wait_for_wire(wire_addr)
        yield http_url, wire_addr
    except BaseException:
        # Drain stderr so the diagnostic surfaces what went wrong
        # before we blow up the process.
        try:
            stderr_bytes = proc.stderr.read() if proc.stderr is not None else b""
            sys.stderr.write(stderr_bytes.decode("utf-8", errors="replace"))
        except Exception:  # pragma: no cover - diagnostic best-effort
            pass
        raise
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        try:
            shutil.rmtree(tempdir, ignore_errors=True)
        except Exception:  # pragma: no cover - cleanup best-effort
            pass
