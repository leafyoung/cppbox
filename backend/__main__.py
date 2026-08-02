"""CPPBox launcher entrypoint: binds 127.0.0.1:<free port>, reuses that exact
socket for uvicorn, and prints `CPPBOX_PORT=<n>` as the first stdout line so a
desktop shell (Tauri) can read the chosen port and open its webview to it.

Run:  python -m backend            (ephemeral free port, localhost only)
      CPPBOX_HOST=0.0.0.0 python -m backend     (LAN/dev access)
      CPPBOX_PORT=8000 python -m backend        (fixed port for dev)
"""
import os
import socket
import sys

import uvicorn


def main():
    host = os.environ.get("CPPBOX_HOST", "127.0.0.1")   # localhost-only by default
    requested = int(os.environ.get("CPPBOX_PORT", "0") or "0")  # 0 = OS picks a free one
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind((host, requested))
    port = sock.getsockname()[1]
    # First line of stdout: the port the launcher should connect to.
    sys.stdout.write(f"CPPBOX_PORT={port}\n")
    sys.stdout.flush()
    config = uvicorn.Config("backend.main:app", host=host, port=port, log_level="info")
    uvicorn.Server(config).run(sockets=[sock])


if __name__ == "__main__":
    main()
