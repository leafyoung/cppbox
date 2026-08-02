"""Disk-backed project storage. Each project = a folder under PROJECTS_ROOT.
Files (incl. subdirs) live on disk so VS Code Remote-SSH edits the same tree.
"""
import os
import shutil
from datetime import datetime, timezone
from pathlib import Path

PROJECTS_ROOT = Path(os.path.dirname(os.path.dirname(__file__))) / "projects"
PROJECTS_ROOT.mkdir(parents=True, exist_ok=True)


def project_root(project_id: str, local_path: str | None = None) -> Path:
    """Return the project's file root.
    If local_path is set, use that (for Google Drive sync mode).
    Otherwise fall back to projects/<id>/."""
    if local_path:
        return Path(local_path).resolve()
    return PROJECTS_ROOT / project_id


def project_dir(project_id: str) -> Path:
    return PROJECTS_ROOT / project_id


def submission_commit(project_id: str, local_path: str | None, seq: int, student_name: str | None) -> str | None:
    """Snapshot the project as a git commit. Returns the commit hash, or None.
    Commit message: 'Submission #{seq} {ISO-8601 ms, UTC Z}'."""
    import subprocess
    d = project_root(project_id, local_path)
    if not d.exists():
        return None
    ts = datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    msg = f"Submission #{seq} {ts}"
    author = student_name or "Student"
    try:
        subprocess.run(["git", "add", "-A"], cwd=str(d), capture_output=True, timeout=20)
        subprocess.run(
            ["git", "-c", f"user.name={author}", "-c", "user.email=cppbox@localhost",
             "commit", "--allow-empty", "-m", msg],
            cwd=str(d), capture_output=True, timeout=20,
        )
        out = subprocess.run(["git", "rev-parse", "HEAD"], cwd=str(d), capture_output=True, timeout=10)
        return out.stdout.decode().strip() or None
    except Exception:
        return None


def init_project(project_id: str):
    d = project_dir(project_id)
    d.mkdir(parents=True, exist_ok=True)
    return d


def delete_project(project_id: str):
    d = project_dir(project_id)
    if d.exists():
        shutil.rmtree(d, ignore_errors=True)


def safe_join(project_id: str, rel_path: str, local_path: str | None = None) -> Path:
    """Resolve rel_path under the project dir, blocking traversal (../) escapes."""
    base = project_root(project_id, local_path).resolve()
    # rel_path may use / ; normalize and strip leading slashes
    clean = rel_path.lstrip("/").lstrip("./")
    target = (base / clean).resolve()
    if target != base and base not in target.parents:
        raise PermissionError("path escapes project directory")
    return target


def write_file(project_id: str, rel_path: str, content: str, local_path: str | None = None) -> str:
    target = safe_join(project_id, rel_path, local_path=local_path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content)
    return rel_path


def read_file(project_id: str, rel_path: str, local_path: str | None = None) -> str:
    target = safe_join(project_id, rel_path, local_path=local_path)
    if not target.exists():
        raise FileNotFoundError(rel_path)
    return target.read_text()


def delete_path(project_id: str, rel_path: str, local_path: str | None = None):
    target = safe_join(project_id, rel_path, local_path=local_path)
    if target.is_dir():
        shutil.rmtree(target, ignore_errors=True)
    elif target.exists():
        target.unlink()


def move_path(project_id: str, old: str, new: str, local_path: str | None = None) -> str:
    src = safe_join(project_id, old, local_path=local_path)
    dst = safe_join(project_id, new, local_path=local_path)
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.move(str(src), str(dst))
    return new


def build_tree(project_id: str, local_path: str | None = None) -> dict:
    base = project_root(project_id, local_path)
    if not base.exists():
        return {"name": base.name, "type": "dir", "path": "", "children": []}

    def walk(path: Path) -> list:
        entries = []
        for child in sorted(path.iterdir(), key=lambda p: (not p.is_dir(), p.name.lower())):
            if child.name.startswith(".") or child.name == "__pycache__":
                continue
            rel = str(child.relative_to(base))
            if child.is_dir():
                entries.append({"name": child.name, "type": "dir", "path": rel, "children": walk(child)})
            else:
                entries.append({"name": child.name, "type": "file", "path": rel})
        return entries

    return {"name": base.name, "type": "dir", "path": "", "children": walk(base)}


def collect_source_files(project_id: str, local_path: str | None = None) -> list[dict]:
    """Read every source/header file for compilation. Returns [{name(relative), content}]."""
    base = project_root(project_id, local_path)
    out = []
    if not base.exists():
        return out
    for p in sorted(base.rglob("*")):
        if not p.is_file():
            continue
        if p.suffix not in (".cpp", ".cc", ".cxx", ".c", ".h", ".hpp", ".hh", ".hxx"):
            continue
        rel = str(p.relative_to(base))
        out.append({"name": rel, "content": p.read_text()})
    return out


def clangd_config_text(std: str) -> str:
    return (
        "CompileFlags:\n"
        "  Add: [-std=" + std + ", -Wall, -Wextra]\n"
        "Diagnostics:\n"
        "  ClangTidy:\n"
        "    Add: [bugprone-*, performance-*, readability-*, modernize-*]\n"
        "    Remove: [readability-magic-numbers, readability-identifier-length, modernize-use-trailing-return-type, readability-function-cognitive-complexity, bugprone-easily-swappable-parameters]\n"
    )


def write_clangd_config(project_id: str, std: str, local_path: str | None = None):
    """Per-project .clangd: -std + clang-tidy checks."""
    d = project_root(project_id, local_path)
    d.mkdir(parents=True, exist_ok=True)
    (d / ".clangd").write_text(clangd_config_text(std))
_GITIGNORE = """\
.ccls-cache/
.breakpoints
.prettierignore
.replit
*.nix
main
main-debug
Makefile
*.o
app
output/

# Prerequisites
*.d

# Compiled Object files
*.slo
*.lo
*.o
*.obj

# Precompiled Headers
*.gch
*.pch

# Compiled Dynamic libraries
*.so
*.dylib
*.dll

# Fortran module files
*.mod
*.smod

# Compiled Static libraries
*.lai
*.la
*.a
*.lib

# Executables
*.exe
*.out
*.app

# Build artifacts
app
"""


def git_init_project(project_id: str, local_path: str | None = None):
    """Initialize a git repo in the project folder and write .gitignore."""
    d = project_root(project_id, local_path)
    if not d.exists():
        return
    (d / ".gitignore").write_text(_GITIGNORE)
    try:
        import subprocess
        subprocess.run(["git", "init"], cwd=str(d), capture_output=True, timeout=10)
    except Exception:
        pass  # git not available or repo already exists


_MAKEFILE = (
    "# Auto-generated by CPPBox\n"
    "CXX := clang++\n"
    "STD := {std}\n"
    "CXXFLAGS := -std=$(STD) -O2 -Wall -Wextra -pedantic\n"
    "SRCS := $(shell find . -type f \\( -name '*.cpp' -o -name '*.cc' -o -name '*.cxx' \\))\n"
    "HDRS := $(shell find . -type f \\( -name '*.h' -o -name '*.hpp' -o -name '*.hh' \\))\n"
    "DIRS := $(sort $(dir $(SRCS) $(HDRS)) .)\n"
    "INCS := $(patsubst %,-I%,$(DIRS))\n"
    "BIN := app\n"
    "\n"
    "all: $(BIN)\n"
    "$(BIN): $(SRCS)\n"
    "\t$(CXX) $(CXXFLAGS) $(INCS) $(SRCS) -o $(BIN)\n"
    "run: all\n"
    "\t./$(BIN)\n"
    "clean:\n"
    "\trm -f $(BIN)\n"
    ".PHONY: all run clean\n"
)


def write_makefile(project_id: str, std: str, local_path: str | None = None):
    """Per-project Makefile so `make` builds under VS Code Remote-SSH."""
    d = project_root(project_id, local_path)
    d.mkdir(parents=True, exist_ok=True)
    (d / "Makefile").write_text(_MAKEFILE.format(std=std))


SUBMISSIONS_ROOT = Path(os.path.dirname(os.path.dirname(__file__))) / "submissions"
SUBMISSIONS_ROOT.mkdir(parents=True, exist_ok=True)

# file extensions to INCLUDE in a submission  (non-executable source / doc)
_SUBMIT_EXTENSIONS = {
    ".cpp", ".cc", ".cxx", ".c", ".h", ".hpp", ".hh", ".hxx",
    ".md", ".txt", ".json", ".yaml", ".yml", ".toml", ".ini", ".cfg",
    ".py", ".sh", ".cmake", ".qmd", ".tex", ".bib", ".csv", ".xml",
    ".cmake", ".qmd", ".r", ".rmd", ".sty", ".cls", ".dtx", ".ins",
    ".css", ".js", ".ts", ".svg", ".html", ".pdf",
}
_SUBMIT_FILENAMES = {"Makefile", "CMakeLists.txt", "compile_flags.txt",
                       ".clangd", ".editorconfig", ".gitignore"}


def collect_submission_files(project_id: str, local_path: str | None = None) -> list[dict]:
    """Collect non-executable source/doc files for a submission zip."""
    base = project_root(project_id, local_path)
    if not base.exists():
        return []
    out = []
    for p in sorted(base.rglob("*")):
        if not p.is_file():
            continue
        # skip git, dot-directories
        parts = p.relative_to(base).parts
        if any(part.startswith(".") and part != ".clangd" and part != ".gitignore" and part != ".editorconfig" for part in parts):
            continue
        # skip by extension
        if p.suffix.lower() in _SUBMIT_EXTENSIONS or p.name in _SUBMIT_FILENAMES:
            rel = str(p.relative_to(base))
            out.append({"name": rel, "content": p.read_text()})
    return out


def create_submission_zip(
    submission_id: str, key: str, counter: int,
    project_id: str, student_name: str, course: str, cohort: str, slot: int,
    project_title: str, submitted_at: str,
    local_path: str | None = None,
) -> str:
    """Create a submission zip file and return its path.
    Zip filename: {key}+{counter}.zip  stored under SUBMISSIONS_ROOT."""
    import zipfile, io, json

    files = collect_submission_files(project_id, local_path=local_path)
    meta = {
        "key": key, "counter": counter,
        "student_name": student_name, "course": course,
        "cohort": cohort, "slot": slot,
        "project_title": project_title,
        "submitted_at": submitted_at,
        "files": [f["name"] for f in files],
    }
    zip_name = f"{key}+{counter}.zip"
    zip_path = str(SUBMISSIONS_ROOT / zip_name)

    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as zf:
        # write meta first
        zf.writestr("meta.json", json.dumps(meta, indent=2))
        for f in files:
            zf.writestr(f["name"], f["content"])
    return zip_path


def _safe_name(name: str) -> str:
    """Sanitize a student name for use in a folder name."""
    keep = [ch for ch in name.strip() if ch.isalnum() or ch in ("-", "_", " ")]
    out = "".join(keep).strip().replace(" ", "_")
    return out or "student"


def organize_submissions(root_folder: str, zips_folder: str, key_lookup: dict) -> dict:
    """Unpack collected submission zips into root_folder/{serial:02d}-{name}/ sub-folders.
    key_lookup: {key: (serial:int, name:str)}. For each key only the highest-counter
    zip is unpacked. Returns {organized:[...], errors:[...]}."""
    import zipfile, json
    root = Path(root_folder)
    root.mkdir(parents=True, exist_ok=True)
    zdir = Path(zips_folder)
    if not zdir.exists():
        return {"organized": [], "errors": [f"zips folder not found: {zips_folder}"]}
    best = {}  # key -> (counter, zip_path)
    errors = []
    for zp in sorted(zdir.glob("*.zip")):
        try:
            with zipfile.ZipFile(zp) as zf:
                meta = json.loads(zf.read("meta.json"))
            key = meta.get("key")
            counter = int(meta.get("counter", 0))
            if key not in key_lookup:
                errors.append(f"{zp.name}: unknown key (not in this assignment)")
                continue
            if key not in best or counter > best[key][0]:
                best[key] = (counter, zp)
        except Exception as e:
            errors.append(f"{zp.name}: {e}")
    organized = []
    for key, (counter, zp) in best.items():
        serial, name = key_lookup[key]
        folder_name = f"{int(serial):02d}-{_safe_name(name)}"
        target = root / folder_name
        target.mkdir(parents=True, exist_ok=True)
        target_resolved = target.resolve()
        try:
            with zipfile.ZipFile(zp) as zf:
                for member in zf.namelist():
                    if member == "meta.json" or member.endswith("/"):
                        continue
                    # zip-slip guard: extraction must stay inside the student folder
                    dest = (target / member).resolve()
                    if dest != target_resolved and target_resolved not in dest.parents:
                        errors.append(f"{zp.name}: skipping unsafe path {member!r}")
                        continue
                    zf.extract(member, target)
            organized.append({"student": name, "serial": serial, "folder": folder_name, "counter": counter})
        except Exception as e:
            errors.append(f"{zp.name}: {e}")
    organized.sort(key=lambda o: o["serial"])
    return {"organized": organized, "errors": errors}


def scan_workspace(root_folder: str) -> list[dict]:
    """List immediate sub-directories of root_folder as [{name, path}]."""
    root = Path(root_folder)
    if not root.exists():
        return []
    out = []
    for child in sorted(root.iterdir(), key=lambda p: p.name.lower()):
        if child.is_dir() and not child.name.startswith("."):
            out.append({"name": child.name, "path": str(child.resolve())})
    return out
