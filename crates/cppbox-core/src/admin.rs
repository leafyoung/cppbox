//! Admin + submission + marking + vscode routes. Drop-in compatible with the
//! Python backend (same paths, same JSON shapes, {detail:...} errors).
use std::collections::HashMap;
use std::net::UdpSocket;
use std::process::Command;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{Assignment, Class, Marking, Setting, Snippet, Student, Submission, SubmissionKey};
use crate::{remote, routes::{fetch_one, ApiError}, storage, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/admin/settings/worker", get(get_worker_settings).put(put_worker_settings))
        .route("/api/admin/classes", post(create_class).get(list_classes))
        .route("/api/admin/classes/{cid}", get(get_class).delete(delete_class))
        .route("/api/admin/classes/{cid}/students", post(import_students))
        .route("/api/admin/classes/{cid}/assignments", post(create_assignment))
        .route("/api/admin/classes/{cid}/keys", get(list_class_keys))
        .route("/api/admin/assignments/{aid}/pull", post(pull_submissions))
        .route("/api/admin/assignments/{aid}/organize", post(organize))
        .route("/api/admin/assignments/{aid}", put(update_assignment))
        .route("/api/admin/assignments/{aid}/grid", get(grid))
        .route("/api/admin/projects/{pid}/feedback", get(get_feedback).post(save_feedback))
        .route("/api/workspace/open", post(open_workspace))
        .route("/api/admin/keys", get(list_keys).post(create_key))
        .route("/api/submit", post(submit))
        .route("/api/admin/submissions", get(list_submissions))
        .route("/api/admin/submissions/{key}", get(list_key_submissions))
        .route("/api/vscode", get(vscode_info))
}

// ── request bodies ───────────────────────────────────────────────────────
#[derive(Deserialize)] struct ClassCreate { name: String, course: String, cohort: String }
#[derive(Deserialize)] struct StudentImport { text: String }
#[derive(Deserialize)] struct AssignmentCreate { name: String, slot: i64, root_folder: Option<String>, expires_ms: Option<i64> }
#[derive(Deserialize)] struct OrganizeRequest { zips_folder: String }
#[derive(Deserialize)] struct WorkspaceOpen { root_folder: String, assignment_id: Option<String> }
#[derive(Deserialize)] struct FeedbackUpdate { text: String, score: Option<String>, #[serde(default)] publish: bool }
#[derive(Deserialize)] struct AssignmentRootUpdate { root_folder: String, expires_ms: Option<i64> }
#[derive(Deserialize)] struct WorkerSettings { worker_url: Option<String>, worker_secret: Option<String> }
#[derive(Deserialize)] struct KeyCreate { student_name: String, course: String, cohort: String, slot: i64 }
#[derive(Deserialize)] struct SubmitRequest { key: String, project_id: String }

// ── helpers ──────────────────────────────────────────────────────────────
fn now() -> String { chrono::Utc::now().to_rfc3339() }
fn uuid() -> String { uuid::Uuid::new_v4().to_string() }
fn db_err<E: std::fmt::Display>(e: E) -> ApiError { ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()) }

fn class_meta(c: &Class, students: i64, assignments: i64) -> Value {
    json!({ "id": c.id, "name": c.name, "course": c.course, "cohort": c.cohort,
            "students": students, "assignments": assignments })
}

async fn get_setting(db: &sqlx::SqlitePool, key: &str) -> Option<String> {
    sqlx::query_as::<_, Setting>("SELECT * FROM settings WHERE key = ?")
        .bind(key).fetch_optional(db).await.ok()?? .value
}

async fn set_setting(db: &sqlx::SqlitePool, key: &str, value: Option<String>) -> Result<(), ApiError> {
    let exists = sqlx::query_as::<_, Setting>("SELECT * FROM settings WHERE key = ?")
        .bind(key).fetch_optional(db).await.map_err(db_err)?;
    let res = match exists {
        Some(_) => sqlx::query("UPDATE settings SET value = ? WHERE key = ?").bind(value).bind(key).execute(db).await,
        None => sqlx::query("INSERT INTO settings(key,value) VALUES (?,?)").bind(key).bind(value).execute(db).await,
    };
    res.map(|_| ()).map_err(db_err)
}

async fn worker_creds(db: &sqlx::SqlitePool) -> (Option<String>, Option<String>) {
    let url = get_setting(db, "worker_url").await.or_else(|| std::env::var("CPPBOX_WORKER_URL").ok()).filter(|s| !s.is_empty());
    let secret = get_setting(db, "worker_secret").await.or_else(|| std::env::var("CPPBOX_WORKER_SECRET").ok()).filter(|s| !s.is_empty());
    (url, secret)
}

/// Parse one roster line -> (serial, name, email|None), or None if blank.
/// Raises (returned as error string) if no valid numeric serial.
fn parse_student_line(line: &str) -> Result<Option<(i64, String, Option<String>)>, String> {
    let s = line.trim();
    if s.is_empty() { return Ok(None); }
    let (serial_str, rest) = match s.split_once(',') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (s.trim(), ""),
    };
    if serial_str.is_empty() || !serial_str.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("line needs a numeric serial first: {line:?}"));
    }
    let serial: i64 = serial_str.parse().map_err(|_| format!("bad serial: {line:?}"))?;
    let mut email = None;
    let name;
    if let (Some(lt), Some(gt)) = (rest.find('<'), rest.find('>')) {
        if lt < gt {
            name = rest[..lt].trim().to_string();
            email = Some(rest[lt + 1..gt].trim().to_string());
        } else {
            name = rest.to_string();
        }
    } else if let Some((n, e)) = rest.split_once(',') {
        name = n.trim().to_string();
        email = Some(e.trim().to_string());
    } else {
        name = rest.to_string();
    }
    let name = if name.is_empty() {
        email.clone().unwrap_or_default().split('@').next().unwrap_or(&format!("student-{serial}")).to_string()
    } else { name };
    let email = email.filter(|e| !e.is_empty());
    Ok(Some((serial, name, email)))
}

fn lp(s: &Snippet) -> Option<&str> { s.local_path.as_deref().filter(|p| !p.is_empty()) }

// ── worker settings ──────────────────────────────────────────────────────
async fn get_worker_settings(State(st): State<AppState>) -> Json<Value> {
    let url = get_setting(&st.db, "worker_url").await
        .or_else(|| std::env::var("CPPBOX_WORKER_URL").ok()).filter(|s| !s.is_empty());
    let secret_set = get_setting(&st.db, "worker_secret").await
        .or_else(|| std::env::var("CPPBOX_WORKER_SECRET").ok()).filter(|s| !s.is_empty()).is_some();
    Json(json!({ "worker_url": url, "worker_secret_set": secret_set, "configured": url.is_some() && secret_set }))
}

async fn put_worker_settings(State(st): State<AppState>, Json(req): Json<WorkerSettings>) -> Result<Json<Value>, ApiError> {
    if let Some(u) = req.worker_url {
        let v = u.trim();
        let val = if v.is_empty() { None } else { Some(v.to_string()) };
        set_setting(&st.db, "worker_url", val).await?;
    }
    if let Some(s) = req.worker_secret {
        let t = s.trim();
        if !t.is_empty() {
            set_setting(&st.db, "worker_secret", Some(t.to_string())).await?;
        }
    }
    Ok(Json(json!({ "ok": true })))
}

// ── classes ──────────────────────────────────────────────────────────────
async fn create_class(State(st): State<AppState>, Json(req): Json<ClassCreate>) -> Result<Json<Value>, ApiError> {
    let id = uuid();
    sqlx::query("INSERT INTO classes(id,name,course,cohort,created_at) VALUES (?,?,?,?,?)")
        .bind(&id).bind(&req.name).bind(&req.course).bind(&req.cohort).bind(now())
        .execute(&st.db).await.map_err(db_err)?;
    let c = Class { id, name: req.name, course: req.course, cohort: req.cohort, created_at: None };
    Ok(Json(class_meta(&c, 0, 0)))
}

async fn list_classes(State(st): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let classes = sqlx::query_as::<_, Class>("SELECT * FROM classes ORDER BY created_at DESC")
        .fetch_all(&st.db).await.map_err(db_err)?;
    let mut out = Vec::new();
    for c in classes {
        let sc: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM students WHERE class_id = ?").bind(&c.id).fetch_one(&st.db).await.unwrap_or(0);
        let ac: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assignments WHERE class_id = ?").bind(&c.id).fetch_one(&st.db).await.unwrap_or(0);
        out.push(class_meta(&c, sc, ac));
    }
    Ok(Json(out))
}

async fn get_class(State(st): State<AppState>, Path(cid): Path<String>) -> Result<Json<Value>, ApiError> {
    let c = sqlx::query_as::<_, Class>("SELECT * FROM classes WHERE id = ?").bind(&cid)
        .fetch_optional(&st.db).await.map_err(db_err)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Class not found".into()))?;
    let students = sqlx::query_as::<_, Student>("SELECT * FROM students WHERE class_id = ?").bind(&cid)
        .fetch_all(&st.db).await.map_err(db_err)?;
    let assignments = sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE class_id = ?").bind(&cid)
        .fetch_all(&st.db).await.map_err(db_err)?;
    let student_list: Vec<Value> = students.iter().map(|s| json!({
        "id": s.id, "serial": s.serial, "name": s.name, "email": s.email
    })).collect();
    let assignment_list: Vec<Value> = assignments.iter().map(|a| json!({
        "id": a.id, "name": a.name, "slot": a.slot, "root_folder": a.root_folder, "expires_ms": a.expires_ms
    })).collect();
    let mut meta = class_meta(&c, student_list.len() as i64, assignment_list.len() as i64);
    meta.as_object_mut().unwrap().insert("student_list".into(), json!(student_list));
    meta.as_object_mut().unwrap().insert("assignment_list".into(), json!(assignment_list));
    Ok(Json(meta))
}

async fn delete_class(State(st): State<AppState>, Path(cid): Path<String>) -> Result<Json<Value>, ApiError> {
    let exists = sqlx::query_as::<_, Class>("SELECT * FROM classes WHERE id = ?").bind(&cid)
        .fetch_optional(&st.db).await.map_err(db_err)?;
    if exists.is_none() { return Err(ApiError(StatusCode::NOT_FOUND, "Class not found".into())); }
    let mut tx = st.db.begin().await.map_err(db_err)?;
    for q in [
        "DELETE FROM markings WHERE assignment_id IN (SELECT id FROM assignments WHERE class_id = ?)",
        "DELETE FROM submissions WHERE key IN (SELECT key FROM submission_keys WHERE class_id = ?)",
        "DELETE FROM submission_keys WHERE class_id = ?",
        "DELETE FROM assignments WHERE class_id = ?",
        "DELETE FROM students WHERE class_id = ?",
        "DELETE FROM classes WHERE id = ?",
    ] {
        sqlx::query(q).bind(&cid).execute(&mut *tx).await.map_err(db_err)?;
    }
    tx.commit().await.map_err(db_err)?;
    Ok(Json(json!({ "ok": true })))
}

// ── students ─────────────────────────────────────────────────────────────
async fn import_students(State(st): State<AppState>, Path(cid): Path<String>, Json(req): Json<StudentImport>) -> Result<Json<Value>, ApiError> {
    let exists = sqlx::query_as::<_, Class>("SELECT * FROM classes WHERE id = ?").bind(&cid)
        .fetch_optional(&st.db).await.map_err(db_err)?;
    if exists.is_none() { return Err(ApiError(StatusCode::NOT_FOUND, "Class not found".into())); }
    let mut added = 0i64;
    let mut errors: Vec<String> = Vec::new();
    let mut tx = st.db.begin().await.map_err(db_err)?;
    for line in req.text.lines() {
        match parse_student_line(line) {
            Ok(None) => continue,
            Ok(Some((serial, name, email))) => {
                let id = uuid();
                sqlx::query("INSERT INTO students(id,class_id,serial,name,email,created_at) VALUES (?,?,?,?,?,?)")
                    .bind(&id).bind(&cid).bind(serial).bind(&name).bind(&email).bind(now())
                    .execute(&mut *tx).await.map_err(db_err)?;
                added += 1;
            }
            Err(e) => errors.push(e),
        }
    }
    tx.commit().await.map_err(db_err)?;
    Ok(Json(json!({ "added": added, "errors": errors })))
}

// ── assignments + keys ───────────────────────────────────────────────────
async fn create_assignment(State(st): State<AppState>, Path(cid): Path<String>, Json(req): Json<AssignmentCreate>) -> Result<Json<Value>, ApiError> {
    let c = sqlx::query_as::<_, Class>("SELECT * FROM classes WHERE id = ?").bind(&cid)
        .fetch_optional(&st.db).await.map_err(db_err)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Class not found".into()))?;
    let aid = uuid();
    sqlx::query("INSERT INTO assignments(id,class_id,name,slot,root_folder,expires_ms,created_at) VALUES (?,?,?,?,?,?,?)")
        .bind(&aid).bind(&cid).bind(&req.name).bind(req.slot).bind(&req.root_folder).bind(req.expires_ms).bind(now())
        .execute(&st.db).await.map_err(db_err)?;
    // mint a 256-bit key for every student
    let students = sqlx::query_as::<_, Student>("SELECT * FROM students WHERE class_id = ?").bind(&cid)
        .fetch_all(&st.db).await.map_err(db_err)?;
    let mut minted: Vec<String> = Vec::new();
    let mut tx = st.db.begin().await.map_err(db_err)?;
    for s in &students {
        let key = crate::db::new_submission_key();
        sqlx::query("INSERT INTO submission_keys(key,student_name,course,cohort,slot,class_id,student_id,assignment_id,created_at) VALUES (?,?,?,?,?,?,?,?,?)")
            .bind(&key).bind(&s.name).bind(&c.course).bind(&c.cohort).bind(req.slot)
            .bind(&cid).bind(&s.id).bind(&aid).bind(now())
            .execute(&mut *tx).await.map_err(db_err)?;
        minted.push(key);
    }
    tx.commit().await.map_err(db_err)?;
    // push to the submission Worker (best-effort)
    let (wurl, wsecret) = worker_creds(&st.db).await;
    let remote_status = if wurl.is_some() && wsecret.is_some() {
        remote::push_keys(wurl.as_deref(), wsecret.as_deref(), &minted).await
    } else { json!({ "skipped": true }) };
    Ok(Json(json!({
        "id": aid, "name": req.name, "slot": req.slot,
        "keys_generated": students.len(), "remote": remote_status,
    })))
}

async fn list_class_keys(State(st): State<AppState>, Path(cid): Path<String>) -> Result<Json<Vec<Value>>, ApiError> {
    let exists = sqlx::query_as::<_, Class>("SELECT * FROM classes WHERE id = ?").bind(&cid)
        .fetch_optional(&st.db).await.map_err(db_err)?;
    if exists.is_none() { return Err(ApiError(StatusCode::NOT_FOUND, "Class not found".into())); }
    let keys = sqlx::query_as::<_, SubmissionKey>("SELECT * FROM submission_keys WHERE class_id = ?").bind(&cid)
        .fetch_all(&st.db).await.map_err(db_err)?;
    let students: HashMap<String, Student> = sqlx::query_as::<_, Student>("SELECT * FROM students WHERE class_id = ?").bind(&cid)
        .fetch_all(&st.db).await.map_err(db_err)?.into_iter().map(|s| (s.id.clone(), s)).collect();
    let assignments: HashMap<String, Assignment> = sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE class_id = ?").bind(&cid)
        .fetch_all(&st.db).await.map_err(db_err)?.into_iter().map(|a| (a.id.clone(), a)).collect();
    let out: Vec<Value> = keys.iter().map(|k| {
        let email = k.student_id.as_ref().and_then(|id| students.get(id)).and_then(|s| s.email.clone());
        let assignment = k.assignment_id.as_ref().and_then(|id| assignments.get(id)).map(|a| a.name.clone());
        json!({ "key": k.key, "slot": k.slot, "student_name": k.student_name,
                "email": email, "assignment": assignment, "course": k.course, "cohort": k.cohort })
    }).collect();
    Ok(Json(out))
}

// ── pull / organize / root / grid / feedback ─────────────────────────────
async fn pull_submissions(State(st): State<AppState>, Path(aid): Path<String>) -> Result<Json<Value>, ApiError> {
    let a = sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE id = ?").bind(&aid)
        .fetch_optional(&st.db).await.map_err(db_err)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Assignment not found".into()))?;
    // only this assignment's keys; deadline filters by the Worker's receive timestamp
    let keys: std::collections::HashSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT key FROM submission_keys WHERE assignment_id = ?").bind(&aid)
        .fetch_all(&st.db).await.map_err(db_err)?.into_iter().collect();
    let (wurl, wsecret) = worker_creds(&st.db).await;
    let dest = st.root.join("submissions");
    Ok(Json(remote::pull_submissions(wurl.as_deref(), wsecret.as_deref(), &dest, &keys, a.expires_ms).await))
}

async fn organize(State(st): State<AppState>, Path(aid): Path<String>, Json(req): Json<OrganizeRequest>) -> Result<Json<Value>, ApiError> {
    let a = sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE id = ?").bind(&aid)
        .fetch_optional(&st.db).await.map_err(db_err)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Assignment not found".into()))?;
    if a.root_folder.as_deref().filter(|s| !s.is_empty()).is_none() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "Assignment has no root_folder; set one first".into()));
    }
    let keys = sqlx::query_as::<_, SubmissionKey>("SELECT * FROM submission_keys WHERE assignment_id = ?").bind(&aid)
        .fetch_all(&st.db).await.map_err(db_err)?;
    let students: HashMap<String, Student> = sqlx::query_as::<_, Student>("SELECT * FROM students WHERE class_id = ?").bind(&a.class_id)
        .fetch_all(&st.db).await.map_err(db_err)?.into_iter().map(|s| (s.id.clone(), s)).collect();
    let mut key_lookup: HashMap<String, (i64, String)> = HashMap::new();
    for k in &keys {
        if let Some(stu) = k.student_id.as_ref().and_then(|id| students.get(id)) {
            if let Some(serial) = stu.serial {
                key_lookup.insert(k.key.clone(), (serial, stu.name.clone()));
            }
        }
    }
    Ok(Json(storage::organize_submissions(
        a.root_folder.as_deref().unwrap_or(""), &req.zips_folder, &key_lookup)))
}

async fn update_assignment(State(st): State<AppState>, Path(aid): Path<String>, Json(req): Json<AssignmentRootUpdate>) -> Result<Json<Value>, ApiError> {
    let a = sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE id = ?").bind(&aid)
        .fetch_optional(&st.db).await.map_err(db_err)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Assignment not found".into()))?;
    let _ = a;
    let root: Option<&str> = match req.root_folder.trim() {
        "" => None,
        v => Some(v),
    };
    sqlx::query("UPDATE assignments SET root_folder = ?, expires_ms = ? WHERE id = ?")
        .bind(root).bind(req.expires_ms).bind(&aid)
        .execute(&st.db).await.map_err(db_err)?;
    Ok(Json(json!({ "ok": true, "root_folder": root, "expires_ms": req.expires_ms })))
}

async fn open_workspace(State(st): State<AppState>, Json(req): Json<WorkspaceOpen>) -> Result<Json<Value>, ApiError> {
    let subs = storage::scan_workspace(&req.root_folder);
    if subs.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "No sub-folders found (or folder missing)".into()));
    }
    let mut by_serial: HashMap<i64, Student> = HashMap::new();
    let mut assignment: Option<Assignment> = None;
    if let Some(aid) = &req.assignment_id {
        let a = sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE id = ?").bind(aid)
            .fetch_optional(&st.db).await.map_err(db_err)?
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Assignment not found".into()))?;
        let studs = sqlx::query_as::<_, Student>("SELECT * FROM students WHERE class_id = ?").bind(&a.class_id)
            .fetch_all(&st.db).await.map_err(db_err)?;
        for s in studs { if let Some(ser) = s.serial { by_serial.insert(ser, s); } }
        assignment = Some(a);
    }
    let mut projects: Vec<Value> = Vec::new();
    let mut tx = st.db.begin().await.map_err(db_err)?;
    for (name, path) in &subs {
        // idempotent by local_path
        let existing = sqlx::query_as::<_, Snippet>("SELECT * FROM snippets WHERE local_path = ?").bind(path)
            .fetch_optional(&mut *tx).await.map_err(db_err)?;
        let (pid, created_at, updated_at) = match existing {
            Some(s) => (s.id, s.created_at, s.updated_at),
            None => {
                let id = uuid();
                let now = now();
                sqlx::query("INSERT INTO snippets(id,title,local_path,code,language,created_at,updated_at,version,cpp_standard,deleted_at) VALUES (?,?,?,NULL,'cpp',?,?,1,'c++17',NULL)")
                    .bind(&id).bind(name).bind(path).bind(&now).bind(&now).execute(&mut *tx).await.map_err(db_err)?;
                (id, Some(now.clone()), Some(now))
            }
        };
        if let Some(a) = &assignment {
            if let Some(stu) = storage::folder_serial(name).and_then(|s| by_serial.get(&s)) {
                let m = sqlx::query_as::<_, Marking>("SELECT * FROM markings WHERE assignment_id = ? AND student_id = ?")
                    .bind(&a.id).bind(&stu.id).fetch_optional(&mut *tx).await.map_err(db_err)?;
                match m {
                    None => {
                        let mid = uuid();
                        sqlx::query("INSERT INTO markings(id,assignment_id,student_id,project_id,graded,graded_at,score,feedback_file,updated_at) VALUES (?,?,?,?,0,NULL,NULL,'feedback.md',NULL)")
                            .bind(&mid).bind(&a.id).bind(&stu.id).bind(&pid).execute(&mut *tx).await.map_err(db_err)?;
                    }
                    Some(m) if m.project_id.as_deref() != Some(&pid) => {
                        sqlx::query("UPDATE markings SET project_id = ? WHERE id = ?").bind(&pid).bind(&m.id)
                            .execute(&mut *tx).await.map_err(db_err)?;
                    }
                    _ => {}
                }
            }
        }
        projects.push(json!({ "id": pid, "title": name, "cpp_standard": "c++17",
                              "created_at": created_at, "updated_at": updated_at,
                              "deleted_at": Value::Null, "local_path": path }));
    }
    tx.commit().await.map_err(db_err)?;
    Ok(Json(json!({ "opened": projects.len(), "projects": projects })))
}

async fn grid(State(st): State<AppState>, Path(aid): Path<String>) -> Result<Json<Value>, ApiError> {
    let a = sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE id = ?").bind(&aid)
        .fetch_optional(&st.db).await.map_err(db_err)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Assignment not found".into()))?;
    let students = sqlx::query_as::<_, Student>(
        "SELECT * FROM students WHERE class_id = ? ORDER BY COALESCE(serial, 999999), name")
        .bind(&a.class_id).fetch_all(&st.db).await.map_err(db_err)?;
    let markings: HashMap<String, Marking> = sqlx::query_as::<_, Marking>("SELECT * FROM markings WHERE assignment_id = ?").bind(&aid)
        .fetch_all(&st.db).await.map_err(db_err)?.into_iter().map(|m| (m.student_id.clone(), m)).collect();
    let rows: Vec<Value> = students.iter().map(|s| {
        let m = markings.get(&s.id);
        let status = match m {
            Some(m) if m.graded.unwrap_or(false) => "graded",
            Some(_) => "submitted",
            None => "none",
        };
        json!({
            "student_id": s.id, "serial": s.serial, "name": s.name, "email": s.email,
            "status": status, "project_id": m.and_then(|m| m.project_id.clone()),
            "score": m.and_then(|m| m.score.clone()),
            "graded_at": m.and_then(|m| m.graded_at.clone()),
        })
    }).collect();
    Ok(Json(json!({ "assignment": { "id": a.id, "name": a.name, "root_folder": a.root_folder }, "students": rows })))
}

async fn get_feedback(State(st): State<AppState>, Path(pid): Path<String>) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    let text = storage::read_file(&st.root, &pid, lp(&s), "feedback.md").unwrap_or_default();
    let m = sqlx::query_as::<_, Marking>("SELECT * FROM markings WHERE project_id = ?").bind(&pid)
        .fetch_optional(&st.db).await.map_err(db_err)?;
    let graded = m.as_ref().map(|m| m.graded.unwrap_or(false)).unwrap_or(false);
    Ok(Json(json!({
        "text": text, "graded": graded,
        "score": m.as_ref().and_then(|m| m.score.clone()),
        "student_id": m.as_ref().map(|m| m.student_id.clone()),
    })))
}

async fn save_feedback(State(st): State<AppState>, Path(pid): Path<String>, Json(req): Json<FeedbackUpdate>) -> Result<Json<Value>, ApiError> {
    let s = fetch_one(&st.db, &pid).await?;
    storage::write_file(&st.root, &pid, lp(&s), "feedback.md", &req.text).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    let m = sqlx::query_as::<_, Marking>("SELECT * FROM markings WHERE project_id = ?").bind(&pid)
        .fetch_optional(&st.db).await.map_err(db_err)?;
    let prev_graded = m.as_ref().map(|m| m.graded.unwrap_or(false)).unwrap_or(false);
    if let Some(m) = &m {
        let now = now();
        let graded: i64 = if req.publish { 1 } else { m.graded.unwrap_or(false) as i64 };
        let graded_at = if req.publish { Some(now.clone()) } else { m.graded_at.clone() };
        sqlx::query("UPDATE markings SET score = ?, graded = ?, graded_at = ?, updated_at = ? WHERE id = ?")
            .bind(&req.score).bind(graded).bind(&graded_at).bind(&now).bind(&m.id)
            .execute(&st.db).await.map_err(db_err)?;
    }
    // graded reflects what was actually persisted: publish only sticks if a marking exists
    let graded = req.publish && m.is_some() || prev_graded;
    let score = if m.is_some() { req.score.clone() } else { None };
    Ok(Json(json!({ "ok": true, "graded": graded, "score": score, "published": req.publish })))
}

// ── keys & submissions ───────────────────────────────────────────────────
async fn list_keys(State(st): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let keys = sqlx::query_as::<_, SubmissionKey>("SELECT * FROM submission_keys")
        .fetch_all(&st.db).await.map_err(db_err)?;
    Ok(Json(keys.iter().map(|k| json!({
        "key": k.key, "student_name": k.student_name, "course": k.course, "cohort": k.cohort, "slot": k.slot
    })).collect()))
}

async fn create_key(State(st): State<AppState>, Json(req): Json<KeyCreate>) -> Result<Json<Value>, ApiError> {
    let key = crate::db::new_submission_key();
    sqlx::query("INSERT INTO submission_keys(key,student_name,course,cohort,slot,class_id,student_id,assignment_id,created_at) VALUES (?,?,?,?,?,NULL,NULL,NULL,?)")
        .bind(&key).bind(&req.student_name).bind(&req.course).bind(&req.cohort).bind(req.slot).bind(now())
        .execute(&st.db).await.map_err(db_err)?;
    Ok(Json(json!({ "key": key, "student_name": req.student_name, "course": req.course, "cohort": req.cohort, "slot": req.slot })))
}

async fn submit(State(st): State<AppState>, Json(req): Json<SubmitRequest>) -> Result<Json<Value>, ApiError> {
    let key_obj = sqlx::query_as::<_, SubmissionKey>("SELECT * FROM submission_keys WHERE key = ?").bind(&req.key)
        .fetch_optional(&st.db).await.map_err(db_err)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Invalid submission key".into()))?;
    let proj = fetch_one(&st.db, &req.project_id).await?;
    let prev: Option<i64> = sqlx::query_scalar("SELECT MAX(counter) FROM submissions WHERE key = ?").bind(&req.key)
        .fetch_one(&st.db).await.map_err(db_err)?;
    let counter = prev.unwrap_or(0) + 1;
    let seq: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM submissions WHERE project_id = ?").bind(&proj.id)
        .fetch_one(&st.db).await.map_err(db_err)?;
    let seq = seq + 1;
    let commit_hash = storage::submission_commit(&st.root, &proj.id, lp(&proj), seq, Some(&key_obj.student_name));
    let files = storage::collect_submission_files(&st.root, &proj.id, lp(&proj));
    let submitted_at = now();
    let submissions_root = st.root.join("submissions");
    let zip_path = storage::create_submission_zip(
        &submissions_root, &req.key, counter, &files,
        &key_obj.student_name, &key_obj.course, &key_obj.cohort,
        key_obj.slot.unwrap_or(0), &proj.title.clone().unwrap_or_default(), &submitted_at,
    ).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let zip_name = zip_path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let id = uuid();
    sqlx::query("INSERT INTO submissions(id,key,counter,project_id,project_title,zip_path,commit_hash,submitted_at) VALUES (?,?,?,?,?,?,?,?)")
        .bind(&id).bind(&req.key).bind(counter).bind(&proj.id).bind(&proj.title).bind(&zip_path.display().to_string()).bind(&commit_hash).bind(&submitted_at)
        .execute(&st.db).await.map_err(db_err)?;
    Ok(Json(json!({ "ok": true, "key": req.key, "counter": counter, "zip": zip_name })))
}

fn submission_json(s: &Submission) -> Value {
    let zip = std::path::Path::new(&s.zip_path).file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    json!({
        "key": s.key, "counter": s.counter, "project_title": s.project_title,
        "zip": zip, "submitted_at": s.submitted_at,
    })
}

async fn list_submissions(State(st): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let subs = sqlx::query_as::<_, Submission>("SELECT * FROM submissions ORDER BY submitted_at DESC")
        .fetch_all(&st.db).await.map_err(db_err)?;
    Ok(Json(subs.iter().map(submission_json).collect()))
}

async fn list_key_submissions(State(st): State<AppState>, Path(key): Path<String>) -> Result<Json<Vec<Value>>, ApiError> {
    let subs = sqlx::query_as::<_, Submission>("SELECT * FROM submissions WHERE key = ? ORDER BY counter DESC").bind(&key)
        .fetch_all(&st.db).await.map_err(db_err)?;
    Ok(Json(subs.iter().map(submission_json).collect()))
}

// ── VS Code Remote-SSH info ──────────────────────────────────────────────
fn detect_ips() -> Vec<String> {
    let mut ips = Vec::new();
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(a) = s.local_addr() {
                ips.push(a.ip().to_string());
            }
        }
    }
    if let Ok(o) = Command::new("hostname").arg("-I").output() {
        for tok in String::from_utf8_lossy(&o.stdout).split_whitespace() {
            ips.push(tok.to_string());
        }
    }
    let mut seen = std::collections::HashSet::new();
    ips.into_iter().filter(|ip| !ip.starts_with("127.") && seen.insert(ip.clone())).collect()
}

async fn vscode_info(State(st): State<AppState>) -> Json<Value> {
    let host = std::env::var("CPPBOX_HOST").ok()
        .or_else(|| Command::new("hostname").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok().map(|s| s.trim().to_string())))
        .unwrap_or_else(|| "localhost".into());
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    let projects_root = st.root.join("projects").display().to_string();
    Json(json!({ "ssh_host": host, "ssh_user": user, "ips": detect_ips(), "projects_root": projects_root }))
}
