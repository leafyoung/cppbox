import os
import re
import uuid
import shutil
import subprocess
import asyncio
from pathlib import Path

# Registry-qualified ref for the packaged app (set via env); local name for dev.
SANDBOX_IMAGE = os.environ.get("CPPBOX_SANDBOX_IMAGE", "cpp-sandbox:latest")
WORKDIR = os.path.join(os.path.dirname(os.path.dirname(__file__)), "workdir")


def _runtime() -> str:
    """Container CLI: prefer podman (rootless), fall back to docker."""
    if shutil.which("podman"):
        return "podman"
    if shutil.which("docker"):
        return "docker"
    return "podman"  # let `run` fail with a clear message if neither exists


_RUNTIME = _runtime()


def ensure_sandbox_image() -> None:
    """Best-effort prefetch of the sandbox image. Registry-qualified refs
    auto-pull on `run` anyway; this just avoids a first-compile stall.
    Errors (offline, etc.) are ignored — the compile path reports clearly."""
    try:
        if subprocess.run([_RUNTIME, "image", "inspect", SANDBOX_IMAGE],
                          capture_output=True).returncode != 0:
            subprocess.run([_RUNTIME, "pull", SANDBOX_IMAGE], capture_output=True)
    except Exception:
        pass


class CompileResult:
    def __init__(self, success: bool, output: str, binary_path: str | None = None):
        self.success = success
        self.output = output
        self.binary_path = binary_path


class RunResult:
    def __init__(self, output: str, exit_code: int, timed_out: bool = False):
        self.output = output
        self.exit_code = exit_code
        self.timed_out = timed_out


async def compile_files(files: list[dict], std: str = "c++17") -> CompileResult:
    """Compile multiple files together. Each file: {name, content}."""
    job_id = uuid.uuid4().hex[:12]
    job_dir = Path(WORKDIR) / job_id
    job_dir.mkdir(parents=True, exist_ok=True)
    os.chmod(job_dir, 0o777)

    # Write all files
    for f in files:
        p = job_dir / f["name"]
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(f["content"])
        p.chmod(0o666)

    # Determine source file list
    all_sources = " ".join(
        f["name"] for f in files if f["name"].endswith((".cpp", ".cc", ".cxx", ".c"))
    )
    if not all_sources:
        all_sources = " ".join(f["name"] for f in files)

    # include every directory (relative) so headers in subdirs resolve
    inc_dirs = {"."}
    for f in files:
        parent = os.path.dirname(f["name"])
        while parent:
            inc_dirs.add(parent)
            parent = os.path.dirname(parent)
    inc_flags = " ".join(f"-I{d}" for d in sorted(inc_dirs))

    binary = job_dir / "a.out"

    try:
        proc = await asyncio.create_subprocess_exec(
            _RUNTIME, "run", "--rm",
            "--network", "none",
            "--memory", "512m",
            "--cpus", "2",
            "--pids-limit", "50",
            "--read-only",
            "--security-opt", "label=disable",
            "-v", f"{job_dir}:/home/sandbox/work:rw",
            "-w", "/home/sandbox/work",
            SANDBOX_IMAGE,
            "sh", "-c",
            f"clang++ -std={std} -O2 -Wall -Wextra -pedantic -fcolor-diagnostics {inc_flags} {all_sources} -o a.out 2>&1",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=60)
        output = (stdout + stderr).decode("utf-8", errors="replace")

        if proc.returncode == 0 and binary.exists():
            return CompileResult(success=True, output=output, binary_path=str(binary))
        else:
            return CompileResult(success=False, output=output or "Compilation failed (no output)")

    except asyncio.TimeoutError:
        return CompileResult(success=False, output="Compilation timed out (60s)")
    except Exception as e:
        return CompileResult(success=False, output=f"Compilation error: {e}")


async def run_binary(binary_path: str, stdin: str = "", timeout: int = 15) -> RunResult:
    """Run a compiled binary in an isolated Docker container."""
    job_dir = Path(binary_path).parent
    binary_name = Path(binary_path).name

    try:
        proc = await asyncio.create_subprocess_exec(
            _RUNTIME, "run", "--rm", "-i",
            "--network", "none",
            "--memory", "256m",
            "--cpus", "1",
            "--pids-limit", "30",
            "--read-only",
            "--security-opt", "label=disable",
            "--cap-drop", "ALL",
            "--security-opt", "no-new-privileges",
            "-v", f"{job_dir}:/home/sandbox/work:ro",
            "-w", "/home/sandbox/work",
            SANDBOX_IMAGE,
            "sh", "-c",
            f"timeout {timeout} ./{binary_name} 2>&1",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await asyncio.wait_for(
            proc.communicate(input=stdin.encode() if stdin else None),
            timeout=timeout + 10,
        )

        output = (stdout + stderr).decode("utf-8", errors="replace")
        timed_out = proc.returncode == 124
        return RunResult(output=output, exit_code=proc.returncode or 0, timed_out=timed_out)

    except asyncio.TimeoutError:
        return RunResult(output="Execution timed out.", exit_code=-1, timed_out=True)
    except Exception as e:
        return RunResult(output=f"Execution error: {e}", exit_code=-1)


async def compile_and_run_files(files: list[dict], stdin: str = "", std: str = "c++17") -> dict:
    """Compile multiple files then run."""
    compile_result = await compile_files(files, std=std)

    if not compile_result.success:
        if compile_result.binary_path:
            _cleanup(Path(compile_result.binary_path).parent)
        return {
            "ok": False,
            "stage": "compile",
            "compile_output": compile_result.output,
            "run_output": "",
        }

    try:
        run_result = await run_binary(compile_result.binary_path, stdin)
    finally:
        _cleanup(Path(compile_result.binary_path).parent)

    return {
        "ok": run_result.exit_code == 0,
        "stage": "run",
        "compile_output": compile_result.output,
        "run_output": run_result.output,
        "exit_code": run_result.exit_code,
        "timed_out": run_result.timed_out,
    }


async def format_code(code: str, style: str = "LLVM") -> str:
    """Format C++ code using clang-format from the host."""
    try:
        proc = await asyncio.create_subprocess_exec(
            "clang-format", f"--style={style}",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await asyncio.wait_for(
            proc.communicate(input=code.encode()),
            timeout=10,
        )
        if proc.returncode == 0:
            return stdout.decode("utf-8", errors="replace")
        return code  # return original on failure
    except Exception:
        return code


_DIAG_RE = re.compile(r"^(?P<file>[^:]+):(?P<line>\d+):(?P<col>\d+):\s*(?P<sev>error|warning|note|fatal error):\s*(?P<msg>.*)$")


async def check_syntax(files: list[dict], std: str = "c++17", entry: str | None = None) -> list[dict]:
    """Parse-only check on the HOST (no Docker): fast, never executes code.
    Returns [{file, line, col, severity, message}]."""
    job_id = uuid.uuid4().hex[:12]
    job_dir = Path(WORKDIR) / ("chk_" + job_id)
    job_dir.mkdir(parents=True, exist_ok=True)

    for f in files:
        p = job_dir / f["name"]
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(f["content"])

    if entry is None:
        cpp = [f["name"] for f in files if f["name"].endswith((".cpp", ".cc", ".cxx"))]
        entry = cpp[0] if cpp else files[0]["name"]

    # include every directory (absolute) so headers in subdirs resolve
    inc_dirs = {str(job_dir)}
    for f in files:
        parent = os.path.dirname(f["name"])
        while parent:
            inc_dirs.add(str(job_dir / parent))
            parent = os.path.dirname(parent)
    inc_args = []
    for d in sorted(inc_dirs):
        inc_args += ["-I", d]

    try:
        proc = await asyncio.create_subprocess_exec(
            "clang++", f"-std={std}", "-fsyntax-only", "-fno-color-diagnostics",
            "-Wall", "-Wextra", *inc_args,
            str(job_dir / entry),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=20)
        text = (stdout + stderr).decode("utf-8", errors="replace")
    except Exception:
        _cleanup(job_dir)
        return []

    diags = []
    for line in text.splitlines():
        m = _DIAG_RE.match(line)
        if not m:
            continue
        diags.append({
            "file": os.path.basename(m.group("file")),
            "line": int(m.group("line")),
            "col": int(m.group("col")),
            "severity": "error" if "error" in m.group("sev") else "warning" if "warning" in m.group("sev") else "info",
            "message": m.group("msg").strip(),
        })

    _cleanup(job_dir)
    return diags


def _cleanup(job_dir: Path):
    """Remove temp directory."""
    try:
        import shutil
        if job_dir.exists():
            shutil.rmtree(job_dir, ignore_errors=True)
    except Exception:
        pass
