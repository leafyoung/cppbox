//! Core project + file routes. Drop-in compatible with the Python backend:
//! same paths, same JSON shapes, errors return {"detail": ...} like FastAPI.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::Snippet;
use crate::{debug, lsp, sandbox, storage, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/trash", get(list_trash))
        .route(
            "/api/projects/{pid}",
            get(get_project).put(update_project).delete(delete_project),
        )
        .route("/api/projects/{pid}/restore", post(restore_project))
        .route("/api/projects/{pid}/purge", delete(purge_project))
        .route("/api/projects/{pid}/tree", get(get_tree))
        .route(
            "/api/projects/{pid}/file",
            get(read_file).put(write_file).delete(delete_file),
        )
        .route("/api/projects/{pid}/file/raw", get(read_file_raw))
        .route("/api/projects/{pid}/file/move", post(move_file))
        // compile / run / diagnostics / format
        .route("/api/run", post(run_code))
        .route("/api/check", post(check_code))
        .route("/api/format", post(format_code_endpoint))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/projects/{pid}/run", post(run_project))
        .route("/api/projects/{pid}/check", post(check_project))
        .route("/ws/lsp", get(lsp::ws_handler))
        .route("/ws/debug", get(debug::ws_handler))
}

// ── error type ───────────────────────────────────────────────────────────
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "detail": self.1 }))).into_response()
    }
}

// ── request bodies (match Python pydantic schemas) ───────────────────────
#[derive(Deserialize)]
pub struct FileItem {
    pub name: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct RunRequest {
    pub files: Option<Vec<FileItem>>,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub stdin: String,
    #[serde(default = "default_std")]
    pub std: String,
}

#[derive(Deserialize)]
pub struct CheckRequest {
    pub files: Vec<FileItem>,
    #[serde(default = "default_std")]
    pub std: String,
    pub entry: Option<String>,
}

#[derive(Deserialize)]
pub struct FormatRequest {
    pub code: String,
}

#[derive(Deserialize)]
pub struct RunProjectRequest {
    #[serde(default)]
    pub stdin: String,
}

#[derive(Deserialize)]
pub struct ProjectCreate {
    #[serde(default = "default_title")]
    pub title: String,
    #[serde(default = "default_std")]
    pub cpp_standard: String,
    pub main_code: Option<String>,
    pub local_path: Option<String>,
}
fn default_title() -> String {
    "Untitled".into()
}
fn default_std() -> String {
    "c++17".into()
}

#[derive(Deserialize)]
pub struct ProjectUpdate {
    pub title: Option<String>,
    pub cpp_standard: Option<String>,
    pub local_path: Option<String>,
}

#[derive(Deserialize)]
pub struct FileWrite {
    pub path: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub is_dir: bool,
}

#[derive(Deserialize)]
pub struct FileMove {
    pub old_path: String,
    pub new_path: String,
}

#[derive(Deserialize)]
pub struct FilePathQuery {
    pub path: String,
}

// ── helpers ──────────────────────────────────────────────────────────────
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn proj_meta(s: &Snippet) -> Value {
    json!({
        "id": s.id,
        "title": s.title.clone().unwrap_or_else(|| "Untitled".into()),
        "cpp_standard": s.cpp_standard.clone().unwrap_or_else(|| "c++17".into()),
        "created_at": s.created_at,
        "updated_at": s.updated_at,
        "deleted_at": s.deleted_at,
        "local_path": s.local_path,
    })
}

pub async fn fetch_one(db: &sqlx::SqlitePool, id: &str) -> Result<Snippet, ApiError> {
    sqlx::query_as::<_, Snippet>("SELECT * FROM snippets WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Project not found".into()))
}

fn lp(s: &Snippet) -> Option<&str> {
    s.local_path.as_deref().filter(|p| !p.is_empty())
}
// ── handlers ─────────────────────────────────────────────────────────────
async fn list_projects(State(st): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let rows = sqlx::query_as::<_, Snippet>(
        "SELECT * FROM snippets WHERE deleted_at IS NULL ORDER BY updated_at DESC",
    )
    .fetch_all(&st.db)
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(rows.iter().map(proj_meta).collect()))
}

async fn list_trash(State(st): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let rows = sqlx::query_as::<_, Snippet>(
        "SELECT * FROM snippets WHERE deleted_at IS NOT NULL ORDER BY updated_at DESC",
    )
    .fetch_all(&st.db)
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(rows.iter().map(proj_meta).collect()))
}

async fn get_project(
    State(st): State<AppState>,
    Path(pid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    Ok(Json(proj_meta(&s)))
}

async fn create_project(
    State(st): State<AppState>,
    Json(req): Json<ProjectCreate>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
    let title = if req.title.trim().is_empty() {
        "Untitled".to_string()
    } else {
        req.title
    };
    let std = if req.cpp_standard.trim().is_empty() {
        "c++17".to_string()
    } else {
        req.cpp_standard
    };

    sqlx::query(
        "INSERT INTO snippets (id, title, local_path, code, language, created_at, updated_at, version, cpp_standard, deleted_at)
         VALUES (?, ?, ?, NULL, 'cpp', ?, ?, 1, ?, NULL)",
    )
    .bind(&id)
    .bind(&title)
    .bind(&req.local_path)
    .bind(&now)
    .bind(&now)
    .bind(&std)
    .execute(&st.db)
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // create project folder + default main.cpp (mirrors Python init_project)
    let base = storage::project_root(&st.root, &id, req.local_path.as_deref());
    std::fs::create_dir_all(&base)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    storage::write_clangd_config(&st.root, &id, req.local_path.as_deref(), &std);
    storage::write_makefile(&st.root, &id, req.local_path.as_deref(), &std);
    storage::git_init_project(&st.root, &id, req.local_path.as_deref());
    let main_cpp = base.join("main.cpp");
    match req.main_code {
        Some(code) => {
            std::fs::write(&main_cpp, code)
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        None if !main_cpp.exists() => {
            std::fs::write(&main_cpp, "#include <iostream>\n\nint main() {\n    std::cout << \"Hello, CPPBox!\\n\";\n    return 0;\n}\n")
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        None => {}
    }

    let s = fetch_one(&st.db, &id).await?;
    Ok((StatusCode::OK, Json(proj_meta(&s))))
}

async fn update_project(
    State(st): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<ProjectUpdate>,
) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    let title = req
        .title
        .unwrap_or(s.title.unwrap_or_else(|| "Untitled".into()));
    let std_changed = req.cpp_standard.is_some();
    let cpp_standard = req
        .cpp_standard
        .unwrap_or(s.cpp_standard.unwrap_or_else(|| "c++17".into()));
    let local_path = req.local_path.or(s.local_path);
    let now = now_iso();
    sqlx::query("UPDATE snippets SET title = ?, cpp_standard = ?, local_path = ?, updated_at = ? WHERE id = ?")
        .bind(&title)
        .bind(&cpp_standard)
        .bind(&local_path)
        .bind(&now)
        .bind(&pid)
        .execute(&st.db)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // refresh the project's .clangd + Makefile when the standard changes
    if std_changed {
        storage::write_clangd_config(&st.root, &pid, local_path.as_deref(), &cpp_standard);
        storage::write_makefile(&st.root, &pid, local_path.as_deref(), &cpp_standard);
    }
    let s = fetch_one(&st.db, &pid).await?;
    Ok(Json(proj_meta(&s)))
}

async fn delete_project(
    State(st): State<AppState>,
    Path(pid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let now = now_iso();
    let r = sqlx::query("UPDATE snippets SET deleted_at = ? WHERE id = ? AND deleted_at IS NULL")
        .bind(&now)
        .bind(&pid)
        .execute(&st.db)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if r.rows_affected() == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "Project not found".into()));
    }
    Ok(Json(json!({"ok": true})))
}

async fn restore_project(
    State(st): State<AppState>,
    Path(pid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    sqlx::query("UPDATE snippets SET deleted_at = NULL WHERE id = ?")
        .bind(&pid)
        .execute(&st.db)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({"ok": true})))
}

async fn purge_project(
    State(st): State<AppState>,
    Path(pid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    sqlx::query("DELETE FROM snippets WHERE id = ?")
        .bind(&pid)
        .execute(&st.db)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // remove the project folder only if it lives under our projects/ dir
    if lp(&s).is_none() {
        let base = st.root.join("projects").join(&pid);
        let _ = std::fs::remove_dir_all(&base);
    }
    Ok(Json(json!({"ok": true})))
}

async fn get_tree(
    State(st): State<AppState>,
    Path(pid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    Ok(Json(storage::build_tree(&st.root, &pid, lp(&s))))
}

async fn read_file(
    State(st): State<AppState>,
    Path(pid): Path<String>,
    Query(q): Query<FilePathQuery>,
) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    let content = storage::read_file(&st.root, &pid, lp(&s), &q.path)
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "File not found".into()))?;
    Ok(Json(json!({ "path": q.path, "content": content })))
}

async fn write_file(
    State(st): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<FileWrite>,
) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    let res = if req.is_dir {
        storage::make_dir(&st.root, &pid, lp(&s), &req.path)
    } else {
        storage::write_file(&st.root, &pid, lp(&s), &req.path, &req.content)
    };
    res.map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    let now = now_iso();
    let _ = sqlx::query("UPDATE snippets SET updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(&pid)
        .execute(&st.db)
        .await;
    Ok(Json(json!({ "path": req.path })))
}

async fn move_file(
    State(st): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<FileMove>,
) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    storage::move_path(&st.root, &pid, lp(&s), &req.old_path, &req.new_path)
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({ "path": req.new_path })))
}

async fn delete_file(
    State(st): State<AppState>,
    Path(pid): Path<String>,
    Query(q): Query<FilePathQuery>,
) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    storage::delete_path(&st.root, &pid, lp(&s), &q.path)
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({ "ok": true })))
}

// ── compile / run / diagnostics / format ─────────────────────────────────
fn to_files(items: Vec<FileItem>) -> Vec<sandbox::File> {
    items
        .into_iter()
        .map(|f| sandbox::File {
            name: f.name,
            content: f.content,
        })
        .collect()
}

async fn run_code(State(st): State<AppState>, Json(req): Json<RunRequest>) -> Json<Value> {
    let files = match req.files {
        Some(fs) if !fs.is_empty() => to_files(fs),
        _ if !req.code.is_empty() => vec![sandbox::File {
            name: "main.cpp".into(),
            content: req.code,
        }],
        _ => vec![],
    };
    Json(sandbox::compile_and_run(&st.root, &files, &req.stdin, &req.std).await)
}

async fn check_code(State(st): State<AppState>, Json(req): Json<CheckRequest>) -> Json<Value> {
    let files = to_files(req.files);
    let diags = sandbox::check_syntax(&st.root, &files, &req.std, req.entry.as_deref()).await;
    Json(json!({ "diagnostics": diags }))
}

async fn format_code_endpoint(Json(req): Json<FormatRequest>) -> Json<Value> {
    Json(json!({ "formatted": sandbox::format_code(&req.code, "LLVM").await }))
}

/// Raw file bytes (PDF preview etc.). Content-Type set by extension.
async fn read_file_raw(
    State(st): State<AppState>,
    Path(pid): Path<String>,
    Query(q): Query<FilePathQuery>,
) -> Result<axum::response::Response, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    let lp = s.local_path.as_deref().filter(|p| !p.is_empty());
    let target = storage::safe_join(&st.root, &pid, lp, &q.path)
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    let bytes = std::fs::read(&target)
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "File not found".into()))?;
    let ct = if q.path.to_lowercase().ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    };
    Ok(([(axum::http::header::CONTENT_TYPE, ct)], bytes).into_response())
}

// ── user settings (~/.cppbox/cppbox.yaml) ──────────────────────────────
async fn get_settings() -> Json<Value> {
    let s = crate::settings::load();
    Json(json!({
        "theme": s.theme,
        "font_size": s.font_size,
        "indent": s.indent,
        "std": s.std,
        "path": crate::settings::settings_path().display().to_string(),
    }))
}

async fn put_settings(
    Json(req): Json<crate::settings::SettingsFile>,
) -> Result<Json<Value>, ApiError> {
    let p = crate::settings::save(&req)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "ok": true, "path": p.display().to_string() })))
}

async fn run_project(
    State(st): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<RunProjectRequest>,
) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    let src = storage::collect_source_files(&st.root, &pid, lp(&s));
    if src.is_empty() {
        return Ok(Json(
            json!({ "ok": false, "stage": "compile", "compile_output": "No source files found.", "run_output": "" }),
        ));
    }
    // Build with make (incremental; avoids re-compiling unchanged sources) then run ./app
    let lpv = lp(&s).map(str::to_string);
    let std = s.cpp_standard.unwrap_or_else(|| "c++17".into());
    Ok(Json(
        sandbox::make_and_run(&st.root, &pid, lpv.as_deref(), &req.stdin, &std).await,
    ))
}

async fn check_project(
    State(st): State<AppState>,
    Path(pid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    let src = storage::collect_source_files(&st.root, &pid, lp(&s));
    if src.is_empty() {
        return Ok(Json(json!({ "diagnostics": [] })));
    }
    let files: Vec<_> = src
        .into_iter()
        .map(|(n, c)| sandbox::File {
            name: n,
            content: c,
        })
        .collect();
    let std = s.cpp_standard.unwrap_or_else(|| "c++17".into());
    let diags = sandbox::check_syntax(&st.root, &files, &std, None).await;
    Ok(Json(json!({ "diagnostics": diags })))
}
