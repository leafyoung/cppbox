"""clangd LSP bridge: one clangd subprocess per WebSocket session.

Contract (frontend ↔ backend): JSON-RPC objects over WebSocket text frames.
The backend handles Content-Length framing to/from clangd's stdio.
A custom method `$/sync` {std, files} writes project files to the workspace
and returns {workspace} so the frontend can build file:// URIs.
"""
import os
import json
import uuid
import asyncio
import shutil
from pathlib import Path

CLANGD = shutil.which("clangd") or "clangd"
WORKDIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "workdir")

_CONTENT_RE = b"Content-Length:"


class LspSession:
    def __init__(self):
        self.id = uuid.uuid4().hex[:12]
        self.ws_dir = Path(WORKDIR) / ("ws_" + self.id)
        self.proc: asyncio.subprocess.Process | None = None
        self._write_lock = asyncio.Lock()

    async def start(self):
        self.ws_dir.mkdir(parents=True, exist_ok=True)
        os.chmod(self.ws_dir, 0o777)
        self.proc = await asyncio.create_subprocess_exec(
            CLANGD, "--log=error", "--pch-storage=memory", "--clang-tidy",
            cwd=str(self.ws_dir),
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        # drain stderr so the pipe never fills
        asyncio.create_task(self._drain(self.proc.stderr))

    async def stop(self):
        if self.proc:
            try:
                self.proc.terminate()
                await asyncio.wait_for(self.proc.wait(), timeout=3)
            except Exception:
                try:
                    self.proc.kill()
                except Exception:
                    pass
        shutil.rmtree(self.ws_dir, ignore_errors=True)

    async def handle(self, ws):
        pump = asyncio.create_task(self._pump_to_ws(ws))
        try:
            while True:
                raw = await ws.receive_text()
                msg = json.loads(raw)
                if msg.get("method") == "$/sync":
                    await self._do_sync(ws, msg)
                    continue
                await self._send_to_clangd(msg)
        except Exception:
            pass
        finally:
            pump.cancel()

    async def _do_sync(self, ws, msg):
        params = msg.get("params") or {}
        std = params.get("std", "c++17")
        files = params.get("files") or []
        for f in files:
            p = self.ws_dir / f["name"]
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(f["content"])
        # write .clangd into the bridge workspace so clang-tidy checks are active here too
        from backend.storage import clangd_config_text
        (self.ws_dir / ".clangd").write_text(clangd_config_text(std))
        await ws.send_text(json.dumps({
            "jsonrpc": "2.0", "id": msg.get("id"),
            "result": {"workspace": str(self.ws_dir), "std": std},
        }))

    async def _send_to_clangd(self, msg):
        data = json.dumps(msg).encode("utf-8")
        frame = b"Content-Length: %d\r\n\r\n%s" % (len(data), data)
        async with self._write_lock:
            if self.proc and self.proc.stdin:
                self.proc.stdin.write(frame)
                await self.proc.stdin.drain()

    async def _pump_to_ws(self, ws):
        reader = self.proc.stdout
        while True:
            try:
                header = await reader.readuntil(b"\r\n\r\n")
                length = 0
                for line in header.split(b"\r\n"):
                    if line.lower().startswith(_CONTENT_RE.lower()):
                        length = int(line.split(b":", 1)[1].strip())
                        break
                body = await reader.readexactly(length)
                await ws.send_text(body.decode("utf-8", errors="replace"))
            except (asyncio.IncompleteReadError, asyncio.LimitOverrunError, Exception):
                break

    async def _drain(self, stream):
        try:
            while True:
                chunk = await stream.readline()
                if not chunk:
                    break
        except Exception:
            pass
