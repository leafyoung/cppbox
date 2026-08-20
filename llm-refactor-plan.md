# Refactor Plan — CPPBox

Audit against LLM AI Coding Agent principles: predictable structure, explicit state, flat architecture, regenerability, dead-code deletion, and minimal coupling.

---

## High

### 1. Delete dead `ProjectFile` model and `project_files` table

`backend/models.py` defines `ProjectFile` (and the `Snippet.files` relationship), but files are now stored on disk via `storage.py`. No endpoint reads or writes this table. The unused relationship causes an extra join if `Snippet` is ever loaded with `await db.refresh(s, ["files"])`. Since this never happens in the current code, the model is purely dead weight.

- Delete `ProjectFile` class and `Snippet.files` relationship from `models.py`.
- The `project_files` table won't be created on fresh DBs, and existing tables remain harmless but unreferenced.

### 2. Delete dead single-file compile wrappers

`sandbox.py` has:

- `compile_code(source, filename, std)` — wraps `compile_files` for one file.
- `compile_and_run(source, stdin, filename, std)` — wraps `compile_and_run_files` for one file.

Neither is called from any endpoint. `/api/run` always sends `files`, not `code`. The `/api/projects/{pid}/run` endpoint reads from disk. Delete both functions. If backward compat for the old `/api/run` with inline `code` is desired, inline the logic in the endpoint directly.

### 3. Remove unused `entry` parameter from `compile_files`

`sandbox.py::compile_files()` computes `entry` but never passes it to clang. The compile command compiles **all** `.cpp/.cc/.cxx/.c` files — the `entry` variable is dead. Either:

- Remove the `entry` computation and parameter (if not needed for future use).
- Or document that `entry` is reserved for future use.

The `compile_and_run_files` and `/api/projects/{pid}/run` also pass `entry=None` through. Drop it.

### 4. Remove dead `THEMES` constant in `main.py`

`backend/main.py:47`: `THEMES = ["default", "eclipse", "idea"]` is set at module level but never referenced anywhere in the file. The actual theme config is in the frontend JS. Delete.

### 5. Fix `datetime` import split

`main.py` has `from datetime import datetime, timezone` stuck mid-file between routes (inserted before `delete_project`). Move to the top with the other imports.

### 6. Extract repeated project-lookup helper

Every file/tree/run endpoint repeats this 3-line pattern (20+ occurrences):

```python
res = await db.execute(select(Snippet).where(Snippet.id == pid))
s = res.scalar_one_or_none()
if not s:
    raise HTTPException(404, "Project not found")
```

Extract into one of:

- A `_get_project(pid, db)` async helper that returns the `Snippet` or raises 404.
- A FastAPI `Depends` that does the lookup and injects the `Snippet` object.

This eliminates ~40 lines of identical noise and makes the endpoints readable at a glance.

### 7. Fix `run_project` stdin — query-param vs body mismatch

`main.py`:

```python
async def run_project(pid: str, stdin: str = "", db: AsyncSession = Depends(get_db)):
```

`stdin` is a plain string with default → FastAPI treats it as a **query parameter**, not a body field. But the frontend sends it in the POST body:

```javascript
await api("POST", `/api/projects/${project.id}/run`, { stdin });
```

The body `{stdin}` is **silently ignored** — `stdin` is always `""`. Fix: use a Pydantic model (e.g. `class RunProjectRequest(BaseModel): stdin: str = ""`) or annotate with `Body()`:

```python
async def run_project(pid: str, body: RunProjectRequest, db: ...):
```

This is a real bug: stdin input from the web UI never reaches the compiled program.

---

## Medium

### 8. Single-file wrappers for backward-compat `/api/run`

The `/api/run` endpoint accepts both `code` (inline) and `files` (multi-file). The `code` branch calls the now-dead `compile_and_run`. If that branch is still used (by curl/API consumers), inline the call to `compile_and_run_files` with a single-element `files` list. If no consumer uses the `code` branch, remove it.

### 9. Frontend: inline event handlers → central wiring

The 45KB `index.html` uses inline `onclick`/`onchange`/`oninput` handlers on ~40 elements. This makes renaming a function fragile (must grep every `onclick=..."fnName"` string). Replacing with `element.addEventListener('click', fnName)` in a central init block trades one grep for another, but is standard. Worth doing when the next refactor touches the area, not as standalone work.

### 10. Frontend: split into CSS + JS files

The `<style>` block (~150 lines) and `<script>` block (~800 lines) are embedded in `index.html`. Splitting into `frontend/style.css` and `frontend/app.js` makes each file regenerable in isolation. Low risk, simple.

### 11. `sandbox.py`: add structured logging at compile/run boundaries

The sandbox spins up Docker containers, runs clang, and pipes output — all without a single `logging` call. A `logger.info("compile", extra={"job_id":..., "file_count":..., "std":...})` at the start and end of `compile_files()` and `run_binary()` would help debug failures without reading HTTP response bodies. Use `logging` from stdlib (already available). Keep messages compact, structured.

### 12. `storage.py`: tighter `safe_join` path sanitization

`safe_join` uses `rel_path.lstrip("/").lstrip("./")` to strip leading special chars. `lstrip("./")` on a path like `./src/../../etc/passwd` → `src/../../etc/passwd` → `resolve()` normalizes to outside the project → caught by the bounds check. So it works, but the `lstrip` is too permissive for edge cases like `././/` or unicode homoglyphs. Add `os.path.normpath()` after stripping. Low risk.

### 13. `lsp.py`: deferred import inside `_do_sync`

```python
from backend.storage import clangd_config_text
```

is imported at runtime inside `_do_sync`. This is to avoid a circular import. A cleaner fix: move `clangd_config_text` into a separate tiny module (e.g. `backend/config.py`) that both `storage.py` and `lsp.py` can import at top-level without circularity.

### 14. `main.py`: `_detect_ips` misplaced between routes

`_detect_ips` is a bare helper placed between `purge_project` and `vscode_info` — in the middle of the endpoint section. Move it near the top (by `_proj_meta`) or into a utility module.

---

## Lower

### 15. `models.py`: `Snippet` table named `snippets` while representing projects

The table and model are named "snippet" but the app now calls them projects. Renaming to `Project` with `__tablename__ = "projects"` is clean but requires a migration (CREATE TABLE + data copy) or renaming the existing table. Not worth doing until the project is reset or migrated. Note as documentation debt.

### 16. `storage.py:build_tree` root name is project UUID

The tree root's `name` field is the project folder UUID (e.g. `"8c7fa56f-...")`. The frontend never displays the root name (only `children`), so cosmetic. Rename to `"project"` or the project's title from the DB when performance isn't critical.

### 17. Frontend `onTitleChange` fires-and-forgets an async promise

The title input's `onchange="onTitleChange()"` calls an `async function` whose returned promise is unhandled. Any rejection hits the global `unhandledrejection` handler (which now shows it in the UI). Works, but inconsistent with how other async handlers are wrapped. Add `.catch(e => flashErr(e))` or make it synchronous.

### 18. `sandbox.py`: `_cleanup` defers `import shutil`

`shutil` is imported inside `_cleanup`, which runs during error paths. Saves ~0.1ms on the happy path at the cost of readability. Move `import shutil` to the top of the file — it's already imported in `storage.py` and `lsp.py`, so already in the process image.

### 19. `TODO.md` and instruction files

The project has `TODO.md` with deferred items (accurate). No `AGENTS.md` or `CLAUDE.md` exists — the project could benefit from one briefly describing:

- Where files live (projects/<uuid>/)
- How compile works (Docker sandbox)
- How lint works (clangd LSP diagnostics, fallback to host `-fsyntax-only`)
- The trash / restore / purge lifecycle
- The dual storage scheme (DB for metadata, disk for files)
