"""Client for the CPPBox submission Worker (Cloudflare Worker + R2).

Pure transport: every function takes explicit (url, secret) credentials —
resolution (DB Setting -> env fallback) lives in main.py, so nothing about the
Worker endpoint is hardcoded here. Uses only the standard library (no new
dependency). Calls are blocking; invoke via asyncio.to_thread from async code.
"""
import json
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


def configured(url: str | None, secret: str | None) -> bool:
    return bool(url and secret)


def _req(method: str, path: str, secret: str, *, data: bytes | None = None,
         headers: dict | None = None, timeout: int = 20) -> bytes:
    h = {"X-Admin-Secret": secret}
    if headers:
        h.update(headers)
    req = urllib.request.Request(path, data=data, headers=h, method=method)
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def push_keys(url: str | None, secret: str | None, keys: list[str]) -> dict:
    """Append submission keys to the Worker's allowlist. Best-effort."""
    if not url or not secret:
        return {"skipped": True}
    try:
        body = json.dumps({"keys": list(keys)}).encode()
        out = _req("POST", url.rstrip("/") + "/admin/keys", secret, data=body,
                   headers={"Content-Type": "application/json"})
        return json.loads(out)
    except Exception as e:
        return {"error": str(e)}


def pull_submissions(url: str | None, secret: str | None, dest_dir: Path) -> dict:
    """Drain the Worker's R2 queue into dest_dir as *.zip (download + delete each)."""
    if not url or not secret:
        return {"error": "Worker not configured (set URL + secret in Admin → Remote collector)"}
    base = url.rstrip("/")
    try:
        raw = _req("GET", base + "/admin/list", secret)
        objects = json.loads(raw).get("objects", [])
    except Exception as e:
        return {"error": f"list failed: {e}"}
    dest_dir.mkdir(parents=True, exist_ok=True)
    pulled, errors = [], []
    for o in objects:
        name = o.get("name", "")
        if not name:
            continue
        try:
            q = urllib.parse.quote(name, safe="")
            data = _req("GET", base + "/admin/object/" + q, secret, timeout=60)
            (dest_dir / name).write_bytes(data)
            _req("DELETE", base + "/admin/object/" + q, secret, timeout=15)
            pulled.append(name)
        except Exception as e:
            errors.append(f"{name}: {e}")
    return {"pulled": len(pulled), "names": pulled, "errors": errors}
