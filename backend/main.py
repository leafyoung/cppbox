import os
import socket
import threading
from pathlib import Path
from datetime import datetime, timezone
from contextlib import asynccontextmanager
import asyncio
from fastapi import FastAPI, Depends, HTTPException, Query, WebSocket, WebSocketDisconnect
from fastapi.middleware.cors import CORSMiddleware
from fastapi.staticfiles import StaticFiles
from sqlalchemy import select, text, func
from sqlalchemy.ext.asyncio import AsyncSession
from pydantic import BaseModel

from backend.database import get_db, engine
from backend.models import Base, Snippet, SubmissionKey, Submission, Class, Student, Assignment, Marking, Setting
from backend.sandbox import compile_and_run_files, format_code, check_syntax, ensure_sandbox_image
from backend.lsp import LspSession
import backend.storage as storage
import backend.remote as remote
import json


@asynccontextmanager
async def lifespan(app: FastAPI):
    async with engine.begin() as conn:
        await conn.run_sync(Base.metadata.create_all)
        try:
            await conn.execute(text("ALTER TABLE snippets ADD COLUMN cpp_standard VARCHAR(10) DEFAULT 'c++23'"))
        except Exception:
            pass
        try:
            await conn.execute(text("ALTER TABLE snippets ADD COLUMN deleted_at DATETIME"))
        except Exception:
            pass
        try:
            await conn.execute(text("ALTER TABLE snippets ADD COLUMN local_path TEXT"))
        except Exception:
            pass
        for col in ("class_id VARCHAR(36)", "student_id VARCHAR(36)", "assignment_id VARCHAR(36)"):
            try:
                await conn.execute(text(f"ALTER TABLE submission_keys ADD COLUMN {col}"))
            except Exception:
                pass
        try:
            await conn.execute(text("ALTER TABLE submissions ADD COLUMN commit_hash VARCHAR(64)"))
        except Exception:
            pass
        try:
            await conn.execute(text("ALTER TABLE students ADD COLUMN serial INTEGER"))
        except Exception:
            pass
        try:
            await conn.execute(text("ALTER TABLE assignments ADD COLUMN root_folder TEXT"))
        except Exception:
            pass
    # prefetch the sandbox image in the background (don't stall startup)
    threading.Thread(target=ensure_sandbox_image, daemon=True).start()
    yield


app = FastAPI(title="CPPBox", lifespan=lifespan)
app.add_middleware(CORSMiddleware, allow_origins=["*"], allow_methods=["*"], allow_headers=["*"])


# ── Schemas ──────────────────────────────────────────────────────────────

class FileItem(BaseModel):
    name: str
    content: str


class RunRequest(BaseModel):
    files: list[FileItem] | None = None
    code: str = ""
    stdin: str = ""
    std: str = "c++23"


class CheckRequest(BaseModel):
    files: list[FileItem]
    std: str = "c++23"
    entry: str | None = None


class FormatRequest(BaseModel):
    code: str


class ProjectCreate(BaseModel):
    title: str = "Untitled"
    cpp_standard: str = "c++23"
    main_code: str | None = None
    local_path: str | None = None


class ProjectUpdate(BaseModel):
    title: str | None = None
    cpp_standard: str | None = None
    local_path: str | None = None


class FileWrite(BaseModel):
    path: str
    content: str = ""
    is_dir: bool = False


class FileMove(BaseModel):
    old_path: str
    new_path: str


class RunProjectRequest(BaseModel):
    stdin: str = ""


# ── Helpers ──────────────────────────────────────────────────────────────

def _proj_meta(s: Snippet) -> dict:
    return {
        "id": s.id, "title": s.title, "cpp_standard": s.cpp_standard or "c++23",
        "created_at": s.created_at.isoformat() if s.created_at else None,
        "updated_at": s.updated_at.isoformat() if s.updated_at else None,
        "deleted_at": s.deleted_at.isoformat() if s.deleted_at else None,
        "local_path": s.local_path,
    }


async def _get_project(pid: str, db: AsyncSession) -> Snippet:
    res = await db.execute(select(Snippet).where(Snippet.id == pid))
    s = res.scalar_one_or_none()
    if not s:
        raise HTTPException(404, "Project not found")
    return s


def _detect_ips() -> list[str]:
    ips: list[str] = []
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("8.8.8.8", 80))
        ips.append(s.getsockname()[0]); s.close()
    except Exception:
        pass
    try:
        import subprocess
        out = subprocess.check_output(["hostname", "-I"], stderr=subprocess.DEVNULL).decode().split()
        ips.extend(out)
    except Exception:
        pass
    seen = set(); res = []
    for ip in ips:
        if ip and not ip.startswith("127.") and ip not in seen:
            seen.add(ip); res.append(ip)
    return res


# ── Run / Compile / Format (inline, for editor state) ───────────────────

@app.post("/api/run")
async def run_code(req: RunRequest):
    if req.files:
        files_list = [{"name": f.name, "content": f.content} for f in req.files]
    else:
        files_list = [{"name": "main.cpp", "content": req.code}] if req.code else []
    return await compile_and_run_files(files_list, req.stdin, std=req.std)


@app.post("/api/check")
async def check_code(req: CheckRequest):
    diags = await check_syntax(
        [{"name": f.name, "content": f.content} for f in req.files], std=req.std, entry=req.entry,
    )
    return {"diagnostics": diags}


@app.post("/api/format")
async def format_code_endpoint(req: FormatRequest):
    return {"formatted": await format_code(req.code)}


@app.websocket("/ws/lsp")
async def lsp_endpoint(ws: WebSocket):
    await ws.accept()
    session = LspSession()
    await session.start()
    try:
        await session.handle(ws)
    except WebSocketDisconnect:
        pass
    finally:
        await session.stop()


# ── Projects (metadata in DB; files on disk) ────────────────────────────

@app.get("/api/projects")
async def list_projects(db: AsyncSession = Depends(get_db)):
    res = await db.execute(select(Snippet).where(Snippet.deleted_at.is_(None)).order_by(Snippet.updated_at.desc()))
    return [_proj_meta(s) for s in res.scalars().all()]


@app.get("/api/trash")
async def list_trash(db: AsyncSession = Depends(get_db)):
    res = await db.execute(select(Snippet).where(Snippet.deleted_at.is_not(None)).order_by(Snippet.deleted_at.desc()))
    return [_proj_meta(s) for s in res.scalars().all()]


@app.post("/api/projects")
async def create_project(req: ProjectCreate, db: AsyncSession = Depends(get_db)):
    p = Snippet(title=req.title, cpp_standard=req.cpp_standard, local_path=req.local_path)
    db.add(p)
    await db.commit()
    await db.refresh(p)
    storage.init_project(p.id)
    lp = req.local_path
    storage.write_clangd_config(p.id, req.cpp_standard, local_path=lp)
    storage.write_makefile(p.id, req.cpp_standard, local_path=lp)
    storage.git_init_project(p.id, local_path=lp)
    if req.main_code is not None:
        storage.write_file(p.id, "main.cpp", req.main_code, local_path=lp)
    else:
        storage.write_file(p.id, "main.cpp",
                           '#include <iostream>\n\nint main() {\n    std::cout << "Hello, CPPBox!\\n";\n    return 0;\n}\n', local_path=lp)
    return _proj_meta(p)


@app.get("/api/projects/{pid}")
async def get_project(pid: str, db: AsyncSession = Depends(get_db)):
    s = await _get_project(pid, db)
    return _proj_meta(s)


@app.put("/api/projects/{pid}")
async def update_project(pid: str, req: ProjectUpdate, db: AsyncSession = Depends(get_db)):
    s = await _get_project(pid, db)
    if req.title is not None:
        s.title = req.title
    if req.local_path is not None:
        s.local_path = req.local_path
    if req.cpp_standard is not None:
        s.cpp_standard = req.cpp_standard
        storage.write_clangd_config(pid, req.cpp_standard, local_path=s.local_path or None)
        storage.write_makefile(pid, req.cpp_standard, local_path=s.local_path or None)
    await db.commit()
    return _proj_meta(s)


@app.delete("/api/projects/{pid}")
async def delete_project(pid: str, db: AsyncSession = Depends(get_db)):
    """Move to trash (soft delete): keep DB row + files, set deleted_at."""
    s = await _get_project(pid, db)
    s.deleted_at = datetime.now(timezone.utc)
    await db.commit()
    return {"ok": True, "trashed": True}


@app.post("/api/projects/{pid}/restore")
async def restore_project(pid: str, db: AsyncSession = Depends(get_db)):
    s = await _get_project(pid, db)
    s.deleted_at = None
    await db.commit()
    return _proj_meta(s)


@app.delete("/api/projects/{pid}/purge")
async def purge_project(pid: str, db: AsyncSession = Depends(get_db)):
    """Permanently delete: remove DB row + files on disk."""
    s = await _get_project(pid, db)
    await db.delete(s)
    await db.commit()
    storage.delete_project(pid)
    return {"ok": True, "purged": True}


# ── Files on disk (tree / read / write / move / delete) ─────────────────

@app.get("/api/projects/{pid}/tree")
async def get_tree(pid: str, db: AsyncSession = Depends(get_db)):
    s = await _get_project(pid, db)
    return storage.build_tree(pid, local_path=s.local_path)


@app.get("/api/projects/{pid}/file")
async def read_file(pid: str, path: str = Query(...), db: AsyncSession = Depends(get_db)):
    s = await _get_project(pid, db)
    try:
        return {"path": path, "content": storage.read_file(pid, path, local_path=s.local_path)}
    except FileNotFoundError:
        raise HTTPException(404, "File not found")


@app.put("/api/projects/{pid}/file")
async def write_file(pid: str, req: FileWrite, db: AsyncSession = Depends(get_db)):
    s = await _get_project(pid, db)
    if req.is_dir:
        target = storage.safe_join(pid, req.path, local_path=s.local_path)
        target.mkdir(parents=True, exist_ok=True)
    else:
        storage.write_file(pid, req.path, req.content, local_path=s.local_path)
    return {"path": req.path}


@app.post("/api/projects/{pid}/file/move")
async def move_file(pid: str, req: FileMove, db: AsyncSession = Depends(get_db)):
    s = await _get_project(pid, db)
    try:
        storage.move_path(pid, req.old_path, req.new_path, local_path=s.local_path)
    except Exception as e:
        raise HTTPException(400, str(e))
    return {"path": req.new_path}


@app.delete("/api/projects/{pid}/file")
async def delete_file(pid: str, path: str = Query(...), db: AsyncSession = Depends(get_db)):
    s = await _get_project(pid, db)
    storage.delete_path(pid, path, local_path=s.local_path)
    return {"ok": True}


# ── Project-level compile (reads files from disk) ───────────────────────

@app.post("/api/projects/{pid}/run")
async def run_project(pid: str, body: RunProjectRequest, db: AsyncSession = Depends(get_db)):
    s = await _get_project(pid, db)
    files = storage.collect_source_files(pid, local_path=s.local_path)
    if not files:
        return {"ok": False, "stage": "compile", "compile_output": "No source files found.", "run_output": ""}
    return await compile_and_run_files(files, body.stdin, std=s.cpp_standard or "c++23")


@app.post("/api/projects/{pid}/check")
async def check_project(pid: str, db: AsyncSession = Depends(get_db)):
    s = await _get_project(pid, db)
    files = storage.collect_source_files(pid, local_path=s.local_path)
    if not files:
        return {"diagnostics": []}
    return {"diagnostics": await check_syntax(files, std=s.cpp_standard or "c++23")}


# ── Admin: Classes / Students / Assignments ──────────────────────────────

class ClassCreate(BaseModel):
    name: str
    course: str
    cohort: str


class StudentImport(BaseModel):
    text: str  # raw lines: "Name", "Name,email", "Name <email>", or "email"


class AssignmentCreate(BaseModel):
    name: str
    slot: int
    root_folder: str | None = None


class OrganizeRequest(BaseModel):
    zips_folder: str


class WorkspaceOpen(BaseModel):
    root_folder: str
    assignment_id: str | None = None


class FeedbackUpdate(BaseModel):
    text: str
    score: str | None = None
    publish: bool = False


class AssignmentRootUpdate(BaseModel):
    root_folder: str


def _parse_student_line(line: str):
    """Parse one roster line -> (serial:int, name:str, email:str|None), or None if blank.
    Format: 'serial,name,email' | 'serial,name <email>' | 'serial,name'.
    Raises ValueError if the line has no valid numeric serial (serial is never auto-generated)."""
    s = line.strip()
    if not s:
        return None
    serial_str, _, rest = s.partition(",")
    serial_str = serial_str.strip()
    if not serial_str.isdigit():
        raise ValueError(f"line needs a numeric serial first: {line!r}")
    serial = int(serial_str)
    rest = rest.strip()
    email = None
    if "<" in rest and ">" in rest:          # name <email>
        name = rest.split("<")[0].strip()
        email = rest.split("<")[1].split(">")[0].strip()
    elif "," in rest:                        # name,email
        parts = [p.strip() for p in rest.split(",", 1)]
        name, email = parts[0], parts[1]
    else:                                    # name only
        name = rest
    if not name:
        name = (email or "").split("@")[0] or f"student-{serial}"
    return (serial, name, email or None)


def _class_meta(c: Class, student_count: int, assignment_count: int) -> dict:
    return {
        "id": c.id, "name": c.name, "course": c.course, "cohort": c.cohort,
        "students": student_count, "assignments": assignment_count,
    }


# ── Worker credentials: DB Setting -> env fallback (never hardcoded) ──────
async def _get_setting(db: AsyncSession, key: str, default: str | None = None) -> str | None:
    s = (await db.execute(select(Setting).where(Setting.key == key))).scalars().first()
    return s.value if (s and s.value is not None) else default


async def _set_setting(db: AsyncSession, key: str, value: str | None) -> None:
    s = (await db.execute(select(Setting).where(Setting.key == key))).scalars().first()
    if s:
        s.value = value
    else:
        db.add(Setting(key=key, value=value))


async def _worker_creds(db: AsyncSession) -> tuple[str | None, str | None]:
    """Resolve the collector endpoint+secret: DB setting first, then env vars."""
    url = await _get_setting(db, "worker_url") or os.environ.get("CPPBOX_WORKER_URL")
    secret = await _get_setting(db, "worker_secret") or os.environ.get("CPPBOX_WORKER_SECRET")
    return url, secret


class WorkerSettings(BaseModel):
    worker_url: str | None = None
    worker_secret: str | None = None  # set only to change; omitted/blank keeps existing


@app.get("/api/admin/settings/worker")
async def get_worker_settings(db: AsyncSession = Depends(get_db)):
    url = await _get_setting(db, "worker_url") or os.environ.get("CPPBOX_WORKER_URL")
    secret_set = bool(await _get_setting(db, "worker_secret") or os.environ.get("CPPBOX_WORKER_SECRET"))
    return {"worker_url": url, "worker_secret_set": secret_set,
            "configured": bool(url and secret_set)}


@app.put("/api/admin/settings/worker")
async def put_worker_settings(req: WorkerSettings, db: AsyncSession = Depends(get_db)):
    if req.worker_url is not None:
        await _set_setting(db, "worker_url", req.worker_url.strip() or None)
    if req.worker_secret and req.worker_secret.strip():
        # only overwrite when a real value is supplied (blank => keep existing)
        await _set_setting(db, "worker_secret", req.worker_secret.strip())
    await db.commit()
    return await get_worker_settings(db)


@app.post("/api/admin/classes")
async def create_class(req: ClassCreate, db: AsyncSession = Depends(get_db)):
    c = Class(name=req.name, course=req.course, cohort=req.cohort)
    db.add(c)
    await db.commit()
    await db.refresh(c)
    return _class_meta(c, 0, 0)


@app.get("/api/admin/classes")
async def list_classes(db: AsyncSession = Depends(get_db)):
    res = await db.execute(select(Class).order_by(Class.created_at.desc()))
    out = []
    for c in res.scalars().all():
        sc = (await db.execute(select(Student).where(Student.class_id == c.id))).scalars().all()
        ac = (await db.execute(select(Assignment).where(Assignment.class_id == c.id))).scalars().all()
        out.append(_class_meta(c, len(sc), len(ac)))
    return out


@app.get("/api/admin/classes/{cid}")
async def get_class(cid: str, db: AsyncSession = Depends(get_db)):
    res = await db.execute(select(Class).where(Class.id == cid))
    c = res.scalar_one_or_none()
    if not c:
        raise HTTPException(404, "Class not found")
    students = (await db.execute(select(Student).where(Student.class_id == cid))).scalars().all()
    assignments = (await db.execute(select(Assignment).where(Assignment.class_id == cid))).scalars().all()
    return {
        **_class_meta(c, len(students), len(assignments)),
        "student_list": [{"id": s.id, "serial": s.serial, "name": s.name, "email": s.email} for s in students],
        "assignment_list": [{"id": a.id, "name": a.name, "slot": a.slot, "root_folder": a.root_folder} for a in assignments],
    }


@app.delete("/api/admin/classes/{cid}")
async def delete_class(cid: str, db: AsyncSession = Depends(get_db)):
    res = await db.execute(select(Class).where(Class.id == cid))
    c = res.scalar_one_or_none()
    if not c:
        raise HTTPException(404, "Class not found")
    await db.delete(c)
    await db.commit()
    return {"ok": True}


@app.post("/api/admin/classes/{cid}/students")
async def import_students(cid: str, req: StudentImport, db: AsyncSession = Depends(get_db)):
    res = await db.execute(select(Class).where(Class.id == cid))
    c = res.scalar_one_or_none()
    if not c:
        raise HTTPException(404, "Class not found")
    added = 0
    errors = []
    for line in req.text.splitlines():
        try:
            parsed = _parse_student_line(line)
        except ValueError as e:
            errors.append(str(e))
            continue
        if not parsed:
            continue
        serial, name, email = parsed
        db.add(Student(class_id=cid, serial=serial, name=name, email=email))
        added += 1
    await db.commit()
    return {"added": added, "errors": errors}


@app.post("/api/admin/classes/{cid}/assignments")
async def create_assignment(cid: str, req: AssignmentCreate, db: AsyncSession = Depends(get_db)):
    res = await db.execute(select(Class).where(Class.id == cid))
    c = res.scalar_one_or_none()
    if not c:
        raise HTTPException(404, "Class not found")
    a = Assignment(class_id=cid, name=req.name, slot=req.slot, root_folder=req.root_folder)
    db.add(a)
    await db.flush()
    # generate a key for every student in the class
    students = (await db.execute(select(Student).where(Student.class_id == cid))).scalars().all()
    for s in students:
        db.add(SubmissionKey(
            student_name=s.name, course=c.course, cohort=c.cohort, slot=req.slot,
            class_id=cid, student_id=s.id, assignment_id=a.id,
        ))
    await db.commit()
    await db.refresh(a)
    # push the minted keys to the submission Worker's allowlist (best-effort)
    keys = (await db.execute(select(SubmissionKey.key).where(SubmissionKey.assignment_id == a.id))).scalars().all()
    wurl, wsecret = await _worker_creds(db)
    remote_status = await asyncio.to_thread(remote.push_keys, wurl, wsecret, list(keys)) if (wurl and wsecret) else None
    return {
        "id": a.id, "name": a.name, "slot": a.slot,
        "keys_generated": len(students),
        "remote": remote_status or {"skipped": True},
    }


@app.post("/api/admin/assignments/{aid}/pull")
async def pull_submissions(aid: str, db: AsyncSession = Depends(get_db)):
    """Drain the submission Worker's R2 queue into submissions/. The Worker is a
    single queue across assignments; Organize filters by key per assignment."""
    a = (await db.execute(select(Assignment).where(Assignment.id == aid))).scalar_one_or_none()
    if not a:
        raise HTTPException(404, "Assignment not found")
    wurl, wsecret = await _worker_creds(db)
    return await asyncio.to_thread(remote.pull_submissions, wurl, wsecret, storage.SUBMISSIONS_ROOT)


@app.post("/api/admin/assignments/{aid}/organize")
async def organize_assignment(aid: str, req: OrganizeRequest, db: AsyncSession = Depends(get_db)):
    res = await db.execute(select(Assignment).where(Assignment.id == aid))
    a = res.scalar_one_or_none()
    if not a:
        raise HTTPException(404, "Assignment not found")
    if not a.root_folder:
        raise HTTPException(400, "Assignment has no root_folder; set one first")
    # build key -> (serial, name) for this assignment's keys
    keys = (await db.execute(select(SubmissionKey).where(SubmissionKey.assignment_id == aid))).scalars().all()
    student_ids = [k.student_id for k in keys if k.student_id]
    students = {}
    if student_ids:
        students = {s.id: s for s in (await db.execute(select(Student).where(Student.id.in_(student_ids)))).scalars().all()}
    key_lookup = {}
    for k in keys:
        st = students.get(k.student_id)
        if st and st.serial is not None:
            key_lookup[k.key] = (st.serial, st.name)
    return storage.organize_submissions(a.root_folder, req.zips_folder, key_lookup)


@app.put("/api/admin/assignments/{aid}/root")
async def set_assignment_root(aid: str, req: AssignmentRootUpdate, db: AsyncSession = Depends(get_db)):
    a = (await db.execute(select(Assignment).where(Assignment.id == aid))).scalar_one_or_none()
    if not a:
        raise HTTPException(404, "Assignment not found")
    a.root_folder = req.root_folder or None
    await db.commit()
    return {"ok": True, "root_folder": a.root_folder}


def _folder_serial(name: str) -> int | None:
    """Extract the leading serial from a 'NN-name' folder name."""
    head = name.split("-", 1)[0]
    return int(head) if head.isdigit() else None


@app.post("/api/workspace/open")
async def open_workspace(req: WorkspaceOpen, db: AsyncSession = Depends(get_db)):
    """Scan a folder's immediate sub-directories and register each as a
    local_path project (idempotent by local_path). No .clangd/Makefile is
    written so the student's own files are never overwritten. If assignment_id
    is given, each 'NN-name' folder is linked to its student via a Marking row."""
    subs = storage.scan_workspace(req.root_folder)
    if not subs:
        raise HTTPException(400, "No sub-folders found (or folder missing)")
    students_by_serial = {}
    assignment = None
    if req.assignment_id:
        assignment = (await db.execute(select(Assignment).where(Assignment.id == req.assignment_id))).scalar_one_or_none()
        if not assignment:
            raise HTTPException(404, "Assignment not found")
        studs = (await db.execute(select(Student).where(Student.class_id == assignment.class_id))).scalars().all()
        students_by_serial = {s.serial: s for s in studs if s.serial is not None}
    projects = []
    for sub in subs:
        existing = (await db.execute(select(Snippet).where(Snippet.local_path == sub["path"]))).scalar_one_or_none()
        p = existing
        if p is None:
            p = Snippet(title=sub["name"], local_path=sub["path"], cpp_standard="c++23")
            db.add(p)
            await db.flush()
        if assignment is not None:
            st = students_by_serial.get(_folder_serial(sub["name"]))
            if st:
                m = (await db.execute(select(Marking).where(
                    Marking.assignment_id == assignment.id, Marking.student_id == st.id))).scalar_one_or_none()
                if m is None:
                    db.add(Marking(assignment_id=assignment.id, student_id=st.id, project_id=p.id))
                elif m.project_id != p.id:
                    m.project_id = p.id
        projects.append(_proj_meta(p))
    await db.commit()
    return {"opened": len(projects), "projects": projects}


@app.get("/api/admin/assignments/{aid}/grid")
async def assignment_grid(aid: str, db: AsyncSession = Depends(get_db)):
    """Students × grading status for an assignment."""
    a = (await db.execute(select(Assignment).where(Assignment.id == aid))).scalar_one_or_none()
    if not a:
        raise HTTPException(404, "Assignment not found")
    students = (await db.execute(
        select(Student).where(Student.class_id == a.class_id).order_by(Student.serial.nulls_last(), Student.name)
    )).scalars().all()
    markings = {m.student_id: m for m in (await db.execute(
        select(Marking).where(Marking.assignment_id == aid))).scalars().all()}
    rows = []
    for s in students:
        m = markings.get(s.id)
        if m and m.graded:
            status = "graded"
        elif m:
            status = "submitted"
        else:
            status = "none"
        rows.append({
            "student_id": s.id, "serial": s.serial, "name": s.name, "email": s.email,
            "status": status, "project_id": m.project_id if m else None,
            "score": m.score if m else None,
            "graded_at": m.graded_at.isoformat() if (m and m.graded_at) else None,
        })
    return {"assignment": {"id": a.id, "name": a.name, "root_folder": a.root_folder}, "students": rows}


@app.get("/api/admin/projects/{pid}/feedback")
async def get_feedback(pid: str, db: AsyncSession = Depends(get_db)):
    p = await _get_project(pid, db)
    try:
        content = storage.read_file(pid, "feedback.md", local_path=p.local_path)
    except FileNotFoundError:
        content = ""
    m = (await db.execute(select(Marking).where(Marking.project_id == pid))).scalars().first()
    return {
        "text": content, "graded": bool(m and m.graded),
        "score": m.score if m else None, "student_id": m.student_id if m else None,
    }


@app.post("/api/admin/projects/{pid}/feedback")
async def save_project_feedback(pid: str, req: FeedbackUpdate, db: AsyncSession = Depends(get_db)):
    """Save feedback.md. Only flips graded=True when req.publish is set (so a
    draft save does not mark the cell ✅). Score is always overwritten (null clears)."""
    p = await _get_project(pid, db)
    storage.write_file(pid, "feedback.md", req.text, local_path=p.local_path)
    m = (await db.execute(select(Marking).where(Marking.project_id == pid))).scalars().first()
    if m:
        m.score = req.score
        if req.publish:
            m.graded = True
            m.graded_at = datetime.now(timezone.utc)
        await db.commit()
        await db.refresh(m)
    return {"ok": True, "graded": bool(m and m.graded), "score": m.score if m else None, "published": req.publish}


@app.get("/api/admin/classes/{cid}/keys")
async def list_class_keys(cid: str, db: AsyncSession = Depends(get_db)):
    res = await db.execute(select(Class).where(Class.id == cid))
    c = res.scalar_one_or_none()
    if not c:
        raise HTTPException(404, "Class not found")
    keys = (await db.execute(select(SubmissionKey).where(SubmissionKey.class_id == cid))).scalars().all()
    students = {s.id: s for s in (await db.execute(select(Student).where(Student.class_id == cid))).scalars().all()}
    assignments = {a.id: a for a in (await db.execute(select(Assignment).where(Assignment.class_id == cid))).scalars().all()}
    out = []
    for k in keys:
        st = students.get(k.student_id)
        asg = assignments.get(k.assignment_id)
        out.append({
            "key": k.key, "slot": k.slot,
            "student_name": k.student_name,
            "email": st.email if st else None,
            "assignment": asg.name if asg else None,
            "course": k.course, "cohort": k.cohort,
        })
    return out


# ── Keys & Submissions ───────────────────────────────────────────────────

class KeyCreate(BaseModel):
    student_name: str
    course: str
    cohort: str
    slot: int


class SubmitRequest(BaseModel):
    key: str
    project_id: str


@app.get("/api/admin/keys")
async def list_keys(db: AsyncSession = Depends(get_db)):
    res = await db.execute(select(SubmissionKey))
    keys = []
    for k in res.scalars().all():
        keys.append({"key": k.key, "student_name": k.student_name, "course": k.course, "cohort": k.cohort, "slot": k.slot})
    return keys


@app.post("/api/admin/keys")
async def create_key(req: KeyCreate, db: AsyncSession = Depends(get_db)):
    k = SubmissionKey(**req.model_dump())
    db.add(k)
    await db.commit()
    await db.refresh(k)
    return {"key": k.key, "student_name": k.student_name, "course": k.course, "cohort": k.cohort, "slot": k.slot}


@app.post("/api/submit")
async def submit_project(req: SubmitRequest, db: AsyncSession = Depends(get_db)):
    # validate key
    res = await db.execute(select(SubmissionKey).where(SubmissionKey.key == req.key))
    key_obj = res.scalar_one_or_none()
    if not key_obj:
        raise HTTPException(404, "Invalid submission key")
    # validate project
    res = await db.execute(select(Snippet).where(Snippet.id == req.project_id))
    proj = res.scalar_one_or_none()
    if not proj:
        raise HTTPException(404, "Project not found")
    # compute next counter
    cnt_res = await db.execute(
        select(Submission.counter).where(Submission.key == req.key).order_by(Submission.counter.desc()).limit(1)
    )
    prev = cnt_res.scalar()
    counter = (prev or 0) + 1
    # per-project submission sequence (for the git commit message)
    seq_res = await db.execute(select(func.count(Submission.id)).where(Submission.project_id == proj.id))
    seq = (seq_res.scalar() or 0) + 1
    # snapshot the project as a git commit
    commit_hash = storage.submission_commit(proj.id, proj.local_path, seq, key_obj.student_name)
    # get project files and create zip
    submitted_at = datetime.now(timezone.utc).isoformat()
    zip_path = storage.create_submission_zip(
        submission_id="",  # not used yet
        key=req.key, counter=counter,
        project_id=proj.id,
        student_name=key_obj.student_name,
        course=key_obj.course,
        cohort=key_obj.cohort,
        slot=key_obj.slot,
        project_title=proj.title,
        submitted_at=submitted_at,
        local_path=proj.local_path,
    )
    sub = Submission(
        key=req.key, counter=counter,
        project_id=proj.id, project_title=proj.title,
        zip_path=zip_path, commit_hash=commit_hash,
    )
    db.add(sub)
    await db.commit()
    await db.refresh(sub)
    await db.refresh(proj)
    return {
        "ok": True,
        "key": req.key,
        "counter": counter,
        "zip": f"{req.key}+{counter}.zip",
    }


@app.get("/api/admin/submissions")
async def list_submissions(db: AsyncSession = Depends(get_db)):
    from sqlalchemy import desc
    res = await db.execute(select(Submission).order_by(desc(Submission.submitted_at)))
    subs = []
    for sub in res.scalars().all():
        subs.append({
            "key": sub.key, "counter": sub.counter,
            "project_title": sub.project_title,
            "zip": Path(sub.zip_path).name,
            "submitted_at": sub.submitted_at.isoformat() if sub.submitted_at else None,
        })
    return subs


@app.get("/api/admin/submissions/{key}")
async def list_key_submissions(key: str, db: AsyncSession = Depends(get_db)):
    from sqlalchemy import desc
    res = await db.execute(select(Submission).where(Submission.key == key).order_by(desc(Submission.counter)))
    subs = []
    for sub in res.scalars().all():
        subs.append({
            "key": sub.key, "counter": sub.counter,
            "project_title": sub.project_title,
            "zip": Path(sub.zip_path).name,
            "submitted_at": sub.submitted_at.isoformat() if sub.submitted_at else None,
        })
    return subs


# ── VS Code Remote-SSH info ─────────────────────────────────────────────

@app.get("/api/vscode")
async def vscode_info():
    host = os.environ.get("CPPBOX_HOST") or socket.gethostname()
    user = os.environ.get("USER") or "user"
    projects_abs = str(storage.PROJECTS_ROOT)
    return {
        "ssh_host": host,
        "ssh_user": user,
        "ips": _detect_ips(),
        "projects_root": projects_abs,
    }


# ── Serve frontend ───────────────────────────────────────────────────────

app.mount("/", StaticFiles(directory="frontend", html=True), name="frontend")
