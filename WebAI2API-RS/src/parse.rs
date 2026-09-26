//! OpenAI 请求解析，对应原 `src/server/api/openai/parse.js`。
//! 文本模型拼装"虚拟上下文"单条 prompt（中文模板逐字保留），
//! 生图模型只取最后一条 user 消息，图片经 image crate 重编码为 JPEG q90。

use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ParsedRequest {
    pub prompt: String,
    pub image_paths: Vec<PathBuf>,
    pub model_id: String,
    pub model_name: String,
}

#[derive(Debug)]
pub struct ParseError {
    pub code: &'static str,
    pub message: String,
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(Value::as_str) == Some("text") {
                    p.get("text").and_then(Value::as_str).map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn content_images(content: &Value) -> Vec<String> {
    let Value::Array(parts) = content else {
        return Vec::new();
    };
    parts
        .iter()
        .filter_map(|p| {
            if p.get("type").and_then(Value::as_str) == Some("image_url") {
                p.pointer("/image_url/url")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            } else {
                None
            }
        })
        .collect()
}

/// `data:mime;base64,...` → JPEG 文件，文件名 `img_<ms>_<rand>.jpg`（parse.js saveBase64Image）。
fn save_as_jpeg(data_uri: &str, temp_dir: &Path) -> Result<PathBuf, String> {
    let rest = data_uri.strip_prefix("data:").ok_or("不是 data URI")?;
    let (_mime, b64) = rest.split_once(";base64,").ok_or("缺少 base64")?;
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
        .map_err(|e| format!("base64 解码失败: {e}"))?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("图片解码失败: {e}"))?;
    let mut jpeg = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90);
    encoder
        .encode_image(&img)
        .map_err(|e| format!("JPEG 编码失败: {e}"))?;
    let name = format!(
        "img_{}_{}.jpg",
        chrono::Utc::now().timestamp_millis(),
        &uuid_short()
    );
    let path = temp_dir.join(name);
    std::fs::create_dir_all(temp_dir).ok();
    std::fs::write(&path, jpeg).map_err(|e| e.to_string())?;
    Ok(path)
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut s = String::new();
    let mut x = n.max(1);
    for _ in 0..7 {
        let d = x % 36;
        s.push(char::from_digit(d, 36).unwrap());
        x = x / 36 + 7;
    }
    s
}

/// 解析 chat/completions 请求体。`model_type` 由桥的 getModelType 提供（"text" | "image"）。
/// 图片的 base64 解码 / image 解码 / JPEG 编码 / 落盘是 CPU 与 IO 密集操作，
/// 整体放 tokio 阻塞线程池执行；参数与返回值形态与原同步版一致，仅变为 async。
pub async fn parse_request(
    body: &Value,
    model_type: &str,
    image_policy: &str,
    image_limit: u64,
    temp_dir: &Path,
    backend_name: &str,
    model_known: bool,
) -> Result<ParsedRequest, ParseError> {
    let body = body.clone();
    let model_type = model_type.to_string();
    let image_policy = image_policy.to_string();
    let temp_dir = temp_dir.to_path_buf();
    let backend_name = backend_name.to_string();
    let result = tokio::task::spawn_blocking(move || {
        parse_request_sync(
            &body,
            &model_type,
            &image_policy,
            image_limit,
            &temp_dir,
            &backend_name,
            model_known,
        )
    })
    .await;
    match result {
        Ok(r) => r,
        // 仅在闭包 panic 时出现（正常不可达）；按内部错误返回
        Err(e) => Err(ParseError {
            code: "INTERNAL_ERROR",
            message: format!("解析请求任务失败: {e}"),
        }),
    }
}

fn parse_request_sync(
    body: &Value,
    model_type: &str,
    image_policy: &str,
    image_limit: u64,
    temp_dir: &Path,
    backend_name: &str,
    model_known: bool,
) -> Result<ParsedRequest, ParseError> {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or(ParseError {
            code: "NO_MESSAGES",
            message: String::new(),
        })?;
    if messages.is_empty() {
        return Err(ParseError {
            code: "NO_MESSAGES",
            message: String::new(),
        });
    }
    if !messages
        .iter()
        .any(|m| m.get("role").and_then(Value::as_str) == Some("user"))
    {
        return Err(ParseError {
            code: "NO_USER_MESSAGES",
            message: String::new(),
        });
    }
    let model_id = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if !model_id.is_empty() && !model_known {
        return Err(ParseError {
            code: "INVALID_MODEL",
            message: format!("模型无效/后端 {backend_name} 不支持: {model_id}"),
        });
    }

    // 未指定模型时按生图处理（与原版"网页默认"一致）；显式 text 才走文本分支
    if model_type == "text" {
        parse_text(messages, &model_id, image_limit, temp_dir)
    } else {
        parse_image(messages, &model_id, image_policy, image_limit, temp_dir)
    }
}

fn parse_text(
    messages: &[Value],
    model_id: &str,
    image_limit: u64,
    temp_dir: &Path,
) -> Result<ParsedRequest, ParseError> {
    let last_user = messages
        .iter()
        .rposition(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .unwrap();

    let mut prompt = String::new();
    // 第一条 system 消息
    if let Some(sys) = messages
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("system"))
    {
        let text = content_text(sys.get("content").unwrap_or(&Value::Null));
        if !text.is_empty() {
            prompt.push_str(&format!("=== 系统指令 (永远置顶) ===\n{text}\n\n"));
        }
    }
    // 历史：最后一条 user 之前的非 system 消息
    let history: Vec<&Value> = messages
        .iter()
        .take(last_user)
        .filter(|m| m.get("role").and_then(Value::as_str) != Some("system"))
        .collect();
    if !history.is_empty() {
        prompt.push_str("=== 历史对话 (滑动窗口或摘要) ===\n");
        for m in &history {
            let role = if m.get("role").and_then(Value::as_str) == Some("assistant") {
                "AI"
            } else {
                "User"
            };
            prompt.push_str(&format!(
                "{role}: {}\n",
                content_text(m.get("content").unwrap_or(&Value::Null))
            ));
        }
        prompt.push('\n');
    }
    let current = content_text(messages[last_user].get("content").unwrap_or(&Value::Null));
    if prompt.is_empty() {
        prompt.push_str(&current);
    } else {
        prompt.push_str(&format!("=== 当前输入 ===\nUser: {current}"));
    }

    // 图片：不报错，插占位符（parse.js 文本分支）
    let mut image_paths = Vec::new();
    for (i, url) in content_images(messages[last_user].get("content").unwrap_or(&Value::Null))
        .iter()
        .enumerate()
    {
        let n = i + 1;
        if image_paths.len() as u64 >= image_limit {
            prompt.push_str(&format!("\n[图片{n} (已忽略:超过限制)]"));
            continue;
        }
        if !url.starts_with("data:image") {
            prompt.push_str(&format!("\n[图片{n} (无效链接)]"));
            continue;
        }
        match save_as_jpeg(url, temp_dir) {
            Ok(p) => {
                image_paths.push(p);
                prompt.push_str(&format!("\n[图片{n}]"));
            }
            Err(_) => prompt.push_str(&format!("\n[图片{n} (上传失败)]")),
        }
    }
    Ok(ParsedRequest {
        prompt,
        image_paths,
        model_id: model_id.to_string(),
        model_name: model_id.to_string(),
    })
}

fn parse_image(
    messages: &[Value],
    model_id: &str,
    image_policy: &str,
    image_limit: u64,
    temp_dir: &Path,
) -> Result<ParsedRequest, ParseError> {
    let last_user = messages
        .iter()
        .rposition(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .unwrap();
    let content = messages[last_user].get("content").unwrap_or(&Value::Null);
    let prompt = content_text(content);
    let images = content_images(content);

    // imageLimit > 10 时超过 10 张静默忽略，否则报错（parse.js 分支 B）
    let hard_cap = if image_limit > 10 { 10 } else { image_limit };
    if image_limit <= 10 && images.len() as u64 > image_limit {
        return Err(ParseError {
            code: "TOO_MANY_IMAGES",
            message: format!("图片数量超过限制（最大 {image_limit} 张）"),
        });
    }
    let mut image_paths = Vec::new();
    for url in images.iter().take(hard_cap as usize) {
        if !url.starts_with("data:image") {
            continue;
        }
        if let Ok(p) = save_as_jpeg(url, temp_dir) {
            image_paths.push(p);
        }
    }
    // 校验失败即返回：调用方不会入队，先清掉已落盘的临时图片避免泄漏
    if image_policy == "required" && image_paths.is_empty() {
        remove_files(&image_paths);
        return Err(ParseError {
            code: "IMAGE_REQUIRED",
            message: String::new(),
        });
    }
    if image_policy == "forbidden" && !image_paths.is_empty() {
        remove_files(&image_paths);
        return Err(ParseError {
            code: "IMAGE_FORBIDDEN",
            message: String::new(),
        });
    }
    Ok(ParsedRequest {
        prompt,
        image_paths,
        model_id: model_id.to_string(),
        model_name: model_id.to_string(),
    })
}

fn remove_files(paths: &[PathBuf]) {
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_virtual_context() {
        let body = json!({"model": "gpt", "messages": [
            {"role": "system", "content": "你是猫"},
            {"role": "user", "content": "你好"},
            {"role": "assistant", "content": "喵"},
            {"role": "user", "content": "再来"}
        ]});
        let parsed = parse_request_sync(
            &body,
            "text",
            "optional",
            5,
            Path::new("/tmp"),
            "pool",
            true,
        )
        .unwrap();
        assert_eq!(
            parsed.prompt,
            "\
=== 系统指令 (永远置顶) ===\n你是猫\n\n\
=== 历史对话 (滑动窗口或摘要) ===\nUser: 你好\nAI: 喵\n\n\
=== 当前输入 ===\nUser: 再来"
        );
    }

    #[test]
    fn text_plain_when_no_context() {
        let body = json!({"messages": [{"role": "user", "content": "hi"}]});
        let parsed = parse_request_sync(
            &body,
            "text",
            "optional",
            5,
            Path::new("/tmp"),
            "pool",
            true,
        )
        .unwrap();
        assert_eq!(parsed.prompt, "hi");
    }

    #[test]
    fn missing_messages() {
        let err = parse_request_sync(
            &json!({}),
            "text",
            "optional",
            5,
            Path::new("/tmp"),
            "pool",
            true,
        )
        .unwrap_err();
        assert_eq!(err.code, "NO_MESSAGES");
        let err = parse_request_sync(
            &json!({"messages": [{"role": "system", "content": "x"}]}),
            "text",
            "optional",
            5,
            Path::new("/tmp"),
            "pool",
            true,
        )
        .unwrap_err();
        assert_eq!(err.code, "NO_USER_MESSAGES");
        let err = parse_request_sync(
            &json!({"model": "nope", "messages": [{"role": "user", "content": "x"}]}),
            "text",
            "optional",
            5,
            Path::new("/tmp"),
            "pool",
            false,
        )
        .unwrap_err();
        assert_eq!(err.code, "INVALID_MODEL");
        assert!(err.message.contains("模型无效/后端 pool 不支持: nope"));
    }

    #[test]
    fn image_policy_and_limit() {
        let body = json!({"messages": [{"role": "user", "content": "画猫"}]});
        let err = parse_request_sync(
            &body,
            "image",
            "required",
            5,
            Path::new("/tmp"),
            "pool",
            true,
        )
        .unwrap_err();
        assert_eq!(err.code, "IMAGE_REQUIRED");

        let tiny_png = {
            let mut bytes = Vec::new();
            image::RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0]))
                .write_with_encoder(image::codecs::png::PngEncoder::new(&mut bytes))
                .unwrap();
            format!(
                "data:image/png;base64,{}",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
            )
        };
        let with_img = json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "变猫"},
            {"type": "image_url", "image_url": {"url": tiny_png}}
        ]}]});
        let err = parse_request_sync(
            &with_img,
            "image",
            "forbidden",
            5,
            Path::new("/tmp"),
            "pool",
            true,
        )
        .unwrap_err();
        assert_eq!(err.code, "IMAGE_FORBIDDEN");

        let dir = std::env::temp_dir().join(format!("webai2api-parse-{}", std::process::id()));
        let parsed =
            parse_request_sync(&with_img, "image", "optional", 5, &dir, "pool", true).unwrap();
        assert_eq!(parsed.prompt, "变猫");
        assert_eq!(parsed.image_paths.len(), 1);
        assert!(parsed.image_paths[0].extension().unwrap() == "jpg");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
