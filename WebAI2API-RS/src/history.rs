//! 请求历史，SQLite 结构与 SQL 对应原 `src/utils/history.js`，可直接打开现有 history.db。
//! DB 访问（rusqlite 同步 IO）统一放 tokio 阻塞线程池：async 侧只短暂持全局锁
//! clone 出 `Arc<History>` 快照，锁绝不跨越 await 点；Connection 由内部 Mutex 串行化。

use crate::errors::HistoryError;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

struct History {
    conn: Mutex<Connection>,
    media_dir: PathBuf,
}

fn db() -> &'static Mutex<Option<Arc<History>>> {
    static DB: OnceLock<Mutex<Option<Arc<History>>>> = OnceLock::new();
    DB.get_or_init(|| Mutex::new(None))
}

pub fn init(data_dir: &Path) -> Result<(), HistoryError> {
    let history_dir = data_dir.join("history");
    let media_dir = history_dir.join("media");
    fs::create_dir_all(&media_dir)?;
    let conn = Connection::open(history_dir.join("history.db"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS requests (
            id TEXT PRIMARY KEY,
            created_at INTEGER NOT NULL,
            model_id TEXT,
            model_name TEXT,
            prompt TEXT,
            input_images TEXT,
            response_text TEXT,
            reasoning_content TEXT,
            response_media TEXT,
            status TEXT DEFAULT 'pending',
            error_message TEXT,
            duration_ms INTEGER,
            is_streaming INTEGER DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_created_at ON requests(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_status ON requests(status);
        CREATE INDEX IF NOT EXISTS idx_model_id ON requests(model_id);",
    )?;
    *db().lock().unwrap() = Some(Arc::new(History {
        conn: Mutex::new(conn),
        media_dir,
    }));
    crate::logfmt::info("历史记录", "数据库初始化完成");
    Ok(())
}

/// 短暂持锁取快照（Arc clone）；阻塞线程池内执行 DB 闭包时不持有全局锁。
fn db_snapshot() -> Result<Arc<History>, HistoryError> {
    db().lock()
        .unwrap()
        .as_ref()
        .cloned()
        .ok_or(HistoryError::NotInitialized)
}

/// DB 闭包统一放阻塞线程池执行。JoinError 只在闭包 panic 时出现（正常不可达），
/// 映射为错误枚举避免 panic 跨线程扩散。
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, HistoryError> + Send + 'static,
) -> Result<T, HistoryError> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(e.into()))
}

pub struct NewRecord<'a> {
    pub id: &'a str,
    pub model_id: Option<&'a str>,
    pub model_name: Option<&'a str>,
    pub prompt: &'a str,
    pub input_images: Option<&'a str>,
    pub is_streaming: bool,
}

pub async fn create_record(r: &NewRecord<'_>) -> Result<(), HistoryError> {
    let now = chrono::Utc::now().timestamp_millis();
    // 与原版 createRecord 的 `data.modelId || null` 一致：空串落库为 NULL
    let model_id = r.model_id.filter(|s| !s.is_empty()).map(str::to_string);
    let model_name = r.model_name.filter(|s| !s.is_empty()).map(str::to_string);
    let prompt = if r.prompt.is_empty() {
        None
    } else {
        Some(r.prompt.to_string())
    };
    let input_images = r.input_images.map(str::to_string);
    let id = r.id.to_string();
    let is_streaming = r.is_streaming;
    blocking(move || {
        let h = db_snapshot()?;
        let conn = h.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO requests (id, created_at, model_id, model_name, prompt, input_images, status, is_streaming)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
            params![id, now, model_id, model_name, prompt, input_images, is_streaming as i32],
        )
        ?;
        Ok(())
    })
    .await
}

pub struct RecordUpdate<'a> {
    pub status: Option<&'a str>,
    pub response_text: Option<&'a str>,
    pub reasoning_content: Option<&'a str>,
    pub response_media: Option<&'a str>,
    pub error_message: Option<&'a str>,
    pub duration_ms: Option<i64>,
}

/// update_record 的参数先在调用方线程转成 owned（Box<dyn ToSql> 不满足 Send，
/// 不能直接带进 spawn_blocking），SQL 拼装顺序与原版一致。
enum OwnedValue {
    Text(String),
    Int(i64),
}

pub async fn update_record(id: &str, u: &RecordUpdate<'_>) -> Result<(), HistoryError> {
    let mut fields: Vec<&'static str> = Vec::new();
    let mut values: Vec<OwnedValue> = Vec::new();
    if let Some(v) = u.status {
        fields.push("status = ?");
        values.push(OwnedValue::Text(v.to_string()));
    }
    if let Some(v) = u.response_text {
        fields.push("response_text = ?");
        values.push(OwnedValue::Text(v.to_string()));
    }
    if let Some(v) = u.reasoning_content {
        fields.push("reasoning_content = ?");
        values.push(OwnedValue::Text(v.to_string()));
    }
    if let Some(v) = u.response_media {
        fields.push("response_media = ?");
        values.push(OwnedValue::Text(v.to_string()));
    }
    if let Some(v) = u.error_message {
        fields.push("error_message = ?");
        values.push(OwnedValue::Text(v.to_string()));
    }
    if let Some(v) = u.duration_ms {
        fields.push("duration_ms = ?");
        values.push(OwnedValue::Int(v));
    }
    if fields.is_empty() {
        return Ok(());
    }
    let id = id.to_string();
    blocking(move || {
        let h = db_snapshot()?;
        let sql = format!("UPDATE requests SET {} WHERE id = ?", fields.join(", "));
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = values
            .into_iter()
            .map(|v| match v {
                OwnedValue::Text(s) => Box::new(s) as Box<dyn rusqlite::types::ToSql>,
                OwnedValue::Int(n) => Box::new(n) as Box<dyn rusqlite::types::ToSql>,
            })
            .collect();
        params.push(Box::new(id));
        let conn = h.conn.lock().unwrap();
        conn.execute(&sql, rusqlite::params_from_iter(params.iter()))?;
        Ok(())
    })
    .await
}

#[derive(Debug, Clone)]
pub struct ListFilter {
    pub page: u64,
    pub page_size: u64,
    pub status: Option<String>,
    pub model: Option<String>,
    pub search: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
}

fn filter_clause(f: &ListFilter) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut where_sql = String::from(" WHERE 1=1");
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(s) = &f.status {
        if s != "all" && !s.is_empty() {
            where_sql.push_str(" AND status = ?");
            params.push(Box::new(s.clone()));
        }
    }
    if let Some(m) = &f.model {
        if !m.is_empty() {
            where_sql.push_str(" AND model_id LIKE ?");
            params.push(Box::new(format!("%{m}%")));
        }
    }
    if let Some(q) = &f.search {
        if !q.is_empty() {
            where_sql.push_str(" AND (prompt LIKE ? OR response_text LIKE ?)");
            params.push(Box::new(format!("%{q}%")));
            params.push(Box::new(format!("%{q}%")));
        }
    }
    if let Some(d) = &f.start_date {
        if let Some(ms) = date_start_ms(d) {
            where_sql.push_str(" AND created_at >= ?");
            params.push(Box::new(ms));
        }
    }
    if let Some(d) = &f.end_date {
        if let Some(ms) = date_end_ms(d) {
            where_sql.push_str(" AND created_at <= ?");
            params.push(Box::new(ms));
        }
    }
    (where_sql, params)
}

/// 本地时区当天 00:00:00.000 的毫秒时间戳（history.js 用 new Date(startDate) 语义）。
fn date_start_ms(date: &str) -> Option<i64> {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .map(|d| {
            d.and_hms_opt(0, 0, 0)
                .unwrap()
                .and_local_timezone(chrono::Local)
                .unwrap()
                .timestamp_millis()
        })
}
fn date_end_ms(date: &str) -> Option<i64> {
    date_start_ms(date).map(|ms| ms + 24 * 60 * 60 * 1000 - 1)
}

/// 与 JS 版 getList/getDetail 一致：snake_case 键保留原始列值（字符串或 null），
/// camelCase 键解析 JSON（null 降级为 []，与原版 `row.x ? JSON.parse(row.x) : []` 相同）。
fn row_to_json(row: &rusqlite::Row) -> Result<Value, rusqlite::Error> {
    let input_images_raw: Option<String> = row.get(5)?;
    let response_media_raw: Option<String> = row.get(8)?;
    let parse_or_empty = |s: &Option<String>| -> Value {
        s.as_ref()
            .and_then(|v| serde_json::from_str::<Value>(v).ok())
            .unwrap_or(json!([]))
    };
    let input_images = parse_or_empty(&input_images_raw);
    let response_media = parse_or_empty(&response_media_raw);
    Ok(json!({
        "id": row.get::<_, String>(0)?,
        "created_at": row.get::<_, i64>(1)?,
        "model_id": row.get::<_, Option<String>>(2)?,
        "model_name": row.get::<_, Option<String>>(3)?,
        "prompt": row.get::<_, Option<String>>(4)?,
        "input_images": input_images_raw,
        "response_text": row.get::<_, Option<String>>(6)?,
        "reasoning_content": row.get::<_, Option<String>>(7)?,
        "response_media": response_media_raw,
        "status": row.get::<_, Option<String>>(9)?,
        "error_message": row.get::<_, Option<String>>(10)?,
        "duration_ms": row.get::<_, Option<i64>>(11)?,
        "is_streaming": row.get::<_, i64>(12)? == 1,
        "inputImages": input_images,
        "responseMedia": response_media,
        "isStreaming": row.get::<_, i64>(12)? == 1,
    }))
}

const COLUMNS: &str = "id, created_at, model_id, model_name, prompt, input_images, response_text, \
    reasoning_content, response_media, status, error_message, duration_ms, is_streaming";

pub async fn list(f: &ListFilter) -> Result<Value, HistoryError> {
    let f = f.clone();
    blocking(move || {
        let (where_sql, params) = filter_clause(&f);
        let h = db_snapshot()?;
        let conn = h.conn.lock().unwrap();
        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM requests{where_sql}"),
            rusqlite::params_from_iter(params.iter()),
            |r| r.get(0),
        )?;
        let page = f.page.max(1);
        let page_size = f.page_size.clamp(1, 200);
        let offset = (page - 1) * page_size;
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM requests{where_sql} ORDER BY created_at DESC LIMIT ? OFFSET ?"
        ))?;
        let mut all: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let limit = page_size as i64;
        let off = offset as i64;
        all.push(&limit);
        all.push(&off);
        let rows = stmt.query_map(all.as_slice(), row_to_json)?;
        let items: Vec<Value> = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(json!({ "items": items, "total": total, "page": page, "pageSize": page_size }))
    })
    .await
}

pub async fn detail(id: &str) -> Result<Option<Value>, HistoryError> {
    let id = id.to_string();
    blocking(move || {
        let h = db_snapshot()?;
        let conn = h.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM requests WHERE id = ?"))?;
        let mut rows = stmt.query_map([id], row_to_json)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    })
    .await
}

pub async fn stats(f: &ListFilter) -> Result<Value, HistoryError> {
    let f = f.clone();
    blocking(move || {
        let (where_sql, params) = filter_clause(&f);
        let h = db_snapshot()?;
        let conn = h.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT COUNT(*), COALESCE(SUM(status='success'),0), COALESCE(SUM(status='failed'),0), \
                      AVG(duration_ms) FROM requests{where_sql}"),
            rusqlite::params_from_iter(params.iter()),
            |r| {
                Ok(json!({
                    "total": r.get::<_, i64>(0)?,
                    "success": r.get::<_, i64>(1)?,
                    "failed": r.get::<_, i64>(2)?,
                    "avgDuration": r.get::<_, Option<f64>>(3)?,
                }))
            },
        )
        .map_err(HistoryError::from)
    })
    .await
}

pub async fn models() -> Result<Vec<String>, HistoryError> {
    blocking(move || {
        let h = db_snapshot()?;
        let conn = h.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT model_id FROM requests WHERE model_id IS NOT NULL ORDER BY model_id",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(HistoryError::from)
    })
    .await
}

/// 删除记录前先删媒体文件（history.js deleteRecords）。
pub async fn delete_ids(ids: &[String]) -> Result<usize, HistoryError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let ids = ids.to_vec();
    blocking(move || {
        let h = db_snapshot()?;
        let conn = h.conn.lock().unwrap();
        delete_media_for_ids(&conn, &h.media_dir, &ids)?;
        delete_ids_locked(&conn, &ids)
    })
    .await
}

pub async fn delete_by_date_range(start: &str, end: &str) -> Result<usize, HistoryError> {
    let f = ListFilter {
        page: 1,
        page_size: 1,
        status: None,
        model: None,
        search: None,
        start_date: Some(start.to_string()),
        end_date: Some(end.to_string()),
    };
    blocking(move || {
        let (where_sql, params) = filter_clause(&f);
        let h = db_snapshot()?;
        let conn = h.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!("SELECT id FROM requests{where_sql}"))?;
        let ids: Vec<String> = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        delete_media_for_ids(&conn, &h.media_dir, &ids)?;
        delete_ids_locked(&conn, &ids)
    })
    .await
}

fn delete_ids_locked(conn: &Connection, ids: &[String]) -> Result<usize, HistoryError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    conn.execute(
        &format!("DELETE FROM requests WHERE id IN ({placeholders})"),
        rusqlite::params_from_iter(ids.iter()),
    )
    .map_err(HistoryError::from)
}

fn delete_media_for_ids(
    conn: &Connection,
    media_dir: &Path,
    ids: &[String],
) -> Result<(), HistoryError> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT response_media FROM requests WHERE id IN ({placeholders})"
    ))?;
    let media_rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
        r.get::<_, Option<String>>(0)
    })?;
    for row in media_rows.flatten().flatten() {
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&row) {
            for item in items {
                if let Some(path) = item.get("localPath").and_then(Value::as_str) {
                    let path = Path::new(path);
                    // Only remove media managed by this history database.
                    if path.parent() == Some(media_dir) {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn media_dir() -> Option<PathBuf> {
    db().lock().unwrap().as_ref().map(|h| h.media_dir.clone())
}

/// 把 data URI 图片落盘，文件名 `${requestId}_${timestamp}.${ext}`（history.js saveMediaToFile）。
pub async fn save_data_uri(data_uri: &str, request_id: &str) -> Result<Value, HistoryError> {
    let data_uri = data_uri.to_string();
    let request_id = request_id.to_string();
    blocking(move || {
        let rest = data_uri
            .strip_prefix("data:")
            .ok_or(HistoryError::NotDataUri)?;
        let (mime, b64) = rest.split_once(";base64,").ok_or(HistoryError::NotBase64)?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)?;
        let ext = match mime {
            "image/png" => "png",
            "image/jpeg" | "image/jpg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "video/mp4" => "mp4",
            "video/webm" => "webm",
            other => other.split('/').nth(1).unwrap_or("bin"),
        };
        let kind = if mime.starts_with("image/") {
            "image"
        } else if mime.starts_with("video/") {
            "video"
        } else {
            "file"
        };
        let dir = media_dir().ok_or(HistoryError::NotInitialized)?;
        let filename = format!(
            "{request_id}_{}.{ext}",
            chrono::Utc::now().timestamp_millis()
        );
        let path = dir.join(&filename);
        fs::write(&path, &bytes)?;
        Ok(json!({
            "type": kind,
            "mime": mime,
            "localPath": path.to_string_lossy(),
            "filename": filename,
            "size": bytes.len(),
            "status": "downloaded"
        }))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_against_schema() {
        let dir = std::env::temp_dir().join(format!("webai2api-hist-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        init(&dir).unwrap();
        create_record(&NewRecord {
            id: "r1",
            model_id: Some("gemini"),
            model_name: Some("gemini"),
            prompt: "画一只猫",
            input_images: None,
            is_streaming: true,
        })
        .await
        .unwrap();
        update_record(
            "r1",
            &RecordUpdate {
                status: Some("success"),
                response_text: Some("ok"),
                reasoning_content: None,
                response_media: None,
                error_message: None,
                duration_ms: Some(1234),
            },
        )
        .await
        .unwrap();
        let f = ListFilter {
            page: 1,
            page_size: 20,
            status: Some("success".into()),
            model: None,
            search: Some("猫".into()),
            start_date: None,
            end_date: None,
        };
        let page = list(&f).await.unwrap();
        assert_eq!(page["total"], 1);
        assert_eq!(page["items"][0]["is_streaming"], true);
        assert_eq!(page["items"][0]["duration_ms"], 1234);
        let s = stats(&ListFilter {
            status: None,
            model: None,
            search: None,
            start_date: None,
            end_date: None,
            ..f
        })
        .await
        .unwrap();
        assert_eq!(s["success"], 1);
        assert_eq!(models().await.unwrap(), vec!["gemini".to_string()]);
        assert_eq!(delete_ids(&["r1".to_string()]).await.unwrap(), 1);
        assert!(detail("r1").await.unwrap().is_none());

        create_record(&NewRecord {
            id: "r2",
            model_id: None,
            model_name: None,
            prompt: "media",
            input_images: None,
            is_streaming: false,
        })
        .await
        .unwrap();
        let media = save_data_uri("data:image/png;base64,aGVsbG8=", "r2")
            .await
            .unwrap();
        let media_path = PathBuf::from(media["localPath"].as_str().unwrap());
        let media_json = serde_json::json!([media]).to_string();
        update_record(
            "r2",
            &RecordUpdate {
                status: Some("success"),
                response_text: None,
                reasoning_content: None,
                response_media: Some(&media_json),
                error_message: None,
                duration_ms: None,
            },
        )
        .await
        .unwrap();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(delete_by_date_range(&today, &today).await.unwrap(), 1);
        assert!(
            !media_path.exists(),
            "date deletion should remove media file"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
