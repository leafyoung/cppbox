use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};

/// A `snippets` row. Matches the Python/SQLAlchemy schema so this crate reads
/// the same data/cppbox.db the Python backend created (non-destructive A/B test).
#[derive(sqlx::FromRow)]
pub struct Snippet {
    pub id: String,
    pub title: Option<String>,
    pub local_path: Option<String>,
    #[allow(dead_code)]
    pub code: Option<String>,
    #[allow(dead_code)]
    pub language: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    #[allow(dead_code)]
    pub version: Option<i64>,
    pub cpp_standard: Option<String>,
    pub deleted_at: Option<String>,
}

#[derive(sqlx::FromRow)]
pub struct Class {
    pub id: String,
    pub name: String,
    pub course: String,
    pub cohort: String,
    pub created_at: Option<String>,
}

#[derive(sqlx::FromRow)]
pub struct Student {
    pub id: String,
    pub class_id: String,
    pub serial: Option<i64>,
    pub name: String,
    pub email: Option<String>,
    pub created_at: Option<String>,
}

#[derive(sqlx::FromRow)]
pub struct Assignment {
    pub id: String,
    pub class_id: String,
    pub name: String,
    pub slot: Option<i64>,
    pub root_folder: Option<String>,
    pub expires_ms: Option<i64>,
    pub late_policy: Option<String>, // "filter" (default) | "reject"
    pub created_at: Option<String>,
}

#[derive(sqlx::FromRow)]
pub struct SubmissionKey {
    pub key: String,
    pub student_name: String,
    pub course: String,
    pub cohort: String,
    pub slot: Option<i64>,
    pub class_id: Option<String>,
    pub student_id: Option<String>,
    pub assignment_id: Option<String>,
    pub created_at: Option<String>,
}

#[derive(sqlx::FromRow)]
pub struct Submission {
    pub id: String,
    pub key: String,
    pub counter: Option<i64>,
    pub project_id: Option<String>,
    pub project_title: Option<String>,
    pub zip_path: String,
    pub commit_hash: Option<String>,
    pub submitted_at: Option<String>,
}

#[derive(sqlx::FromRow)]
pub struct Marking {
    pub id: String,
    pub assignment_id: String,
    pub student_id: String,
    pub project_id: Option<String>,
    pub graded: Option<bool>,
    pub graded_at: Option<String>,
    pub score: Option<String>,
    pub feedback_file: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(sqlx::FromRow)]
pub struct Setting {
    pub key: String,
    pub value: Option<String>,
}

pub async fn migrate(db: &SqlitePool) -> anyhow::Result<()> {
    // All tables match the Python/SQLAlchemy schema (non-destructive coexistence).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS snippets (
            id TEXT PRIMARY KEY, title TEXT, local_path TEXT, code TEXT, language TEXT,
            created_at TEXT, updated_at TEXT, version INTEGER, cpp_standard TEXT, deleted_at TEXT
        )",
    )
    .execute(db)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS classes (
            id TEXT PRIMARY KEY, name TEXT, course TEXT, cohort TEXT, created_at TEXT
        )",
    )
    .execute(db)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS students (
            id TEXT PRIMARY KEY, class_id TEXT, serial INTEGER, name TEXT, email TEXT, created_at TEXT
        )",
    ).execute(db).await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS assignments (
            id TEXT PRIMARY KEY, class_id TEXT, name TEXT, slot INTEGER, root_folder TEXT, expires_ms INTEGER, late_policy TEXT, created_at TEXT
        )",
    ).execute(db).await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS submission_keys (
            key TEXT PRIMARY KEY, student_name TEXT, course TEXT, cohort TEXT, slot INTEGER,
            class_id TEXT, student_id TEXT, assignment_id TEXT, created_at TEXT
        )",
    )
    .execute(db)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS submissions (
            id TEXT PRIMARY KEY, key TEXT, counter INTEGER, project_id TEXT, project_title TEXT,
            zip_path TEXT, commit_hash TEXT, submitted_at TEXT
        )",
    )
    .execute(db)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS markings (
            id TEXT PRIMARY KEY, assignment_id TEXT, student_id TEXT, project_id TEXT,
            graded INTEGER, graded_at TEXT, score TEXT, feedback_file TEXT, updated_at TEXT,
            UNIQUE (assignment_id, student_id)
        )",
    )
    .execute(db)
    .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT)")
        .execute(db)
        .await?;
    // migration for pre-existing DBs
    let _ = sqlx::query("ALTER TABLE assignments ADD COLUMN expires_ms INTEGER")
        .execute(db)
        .await;
    let _ = sqlx::query("ALTER TABLE assignments ADD COLUMN late_policy TEXT")
        .execute(db)
        .await;
    Ok(())
}

/// Open (creating if missing) the SQLite DB at `path`.
pub async fn connect(path: &std::path::Path) -> anyhow::Result<SqlitePool> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(opts)
        .await?;
    Ok(pool)
}

/// 256-bit submission key from the OS CSPRNG (matches Python new_submission_key).
pub fn new_submission_key() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("CSPRNG unavailable");
    hex_encode(&buf)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}
