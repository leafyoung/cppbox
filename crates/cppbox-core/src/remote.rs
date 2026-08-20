//! Async client for the CPPBox submission Worker (Cloudflare Worker + R2).
//! Pure transport: takes explicit (url, secret). Resolution (DB Setting -> env)
//! lives in admin.rs, so nothing here is hardcoded. Mirrors backend/remote.py.
use std::path::Path;

use serde_json::{json, Value};

fn hdrs(secret: &str) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    if let Ok(v) = reqwest::header::HeaderValue::from_str(secret) {
        h.insert("X-Admin-Secret", v);
    }
    h
}

/// Append the minted keys to the Worker's allowlist. Best-effort.
/// Push keys to the Worker allowlist. Each key carries the assignment's
/// late policy: for "reject", expires_ms is the deadline the Worker enforces;
/// for "filter" (default), expires_ms is null so the Worker accepts everything
/// and the pull filter does the skipping.
pub async fn push_keys(
    url: Option<&str>,
    secret: Option<&str>,
    keys: &[(String, Option<i64>)],
) -> Value {
    let (Some(url), Some(secret)) = (url, secret) else {
        return json!({ "skipped": true });
    };
    let client = reqwest::Client::new();
    let entries: Vec<Value> = keys
        .iter()
        .map(|(k, exp)| json!({ "key": k, "expires_ms": exp }))
        .collect();
    match client
        .post(format!("{}/admin/keys", url.trim_end_matches('/')))
        .headers(hdrs(secret))
        .json(&json!({ "keys": entries }))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
    {
        Ok(r) => r
            .json::<Value>()
            .await
            .unwrap_or_else(|e| json!({ "error": e.to_string() })),
        Err(e) => json!({ "error": e.to_string() }),
    }
}

/// Drain the Worker's R2 queue into `dest` as *.zip (download + delete each).
/// Pull submissions for one assignment only: download objects whose key belongs
/// to `keys` and (if `deadline_ms` is set) were submitted before the deadline.
/// Late/foreign objects are left in R2 untouched.
pub async fn pull_submissions(
    url: Option<&str>,
    secret: Option<&str>,
    dest: &Path,
    keys: &std::collections::HashSet<String>,
    deadline_ms: Option<i64>,
) -> Value {
    let (Some(url), Some(secret)) = (url, secret) else {
        return json!({ "error": "Worker not configured (set URL + secret in Admin → Remote collector)" });
    };
    let base = url.trim_end_matches('/');
    let client = reqwest::Client::new();

    let objects = match client
        .get(format!("{base}/admin/list"))
        .headers(hdrs(secret))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
    {
        Ok(r) => r
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v.get("objects").cloned())
            .unwrap_or(json!([])),
        Err(e) => return json!({ "error": format!("list failed: {e}") }),
    };
    let _ = std::fs::create_dir_all(dest);
    let mut pulled = 0;
    let mut late = 0;
    let mut other = 0;
    let mut names: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for o in objects.as_array().map(|a| a.clone()).unwrap_or_default() {
        let Some(name) = o.get("name").and_then(|n| n.as_str()).map(String::from) else {
            continue;
        };
        // object name is {key}+{ms}.zip — ms is the Worker's receive timestamp
        let stem = name.strip_suffix(".zip").unwrap_or(&name);
        let (key, ms) = match stem.split_once('+') {
            Some((k, m)) => (k.to_string(), m.parse::<i64>().unwrap_or(0)),
            None => {
                other += 1;
                continue;
            }
        };
        if !keys.contains(&key) {
            other += 1;
            continue;
        }
        if let Some(d) = deadline_ms {
            if ms > d {
                late += 1;
                continue;
            } // past deadline — keep in R2
        }
        let q = urlencode(&name);
        let get_url = format!("{base}/admin/object/{q}");
        let res = client
            .get(&get_url)
            .headers(hdrs(secret))
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => match r.bytes().await {
                Ok(b) => {
                    let _ = std::fs::write(dest.join(&name), &b);
                    let _ = client
                        .delete(format!("{base}/admin/object/{q}"))
                        .headers(hdrs(secret))
                        .timeout(std::time::Duration::from_secs(15))
                        .send()
                        .await;
                    pulled += 1;
                    names.push(name);
                }
                Err(e) => errors.push(format!("{name}: {e}")),
            },
            Ok(r) => errors.push(format!("{name}: HTTP {}", r.status())),
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    json!({ "pulled": pulled, "skipped_late": late, "skipped_other": other, "names": names, "errors": errors })
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{:02X}", b)
            }
        })
        .collect()
}
