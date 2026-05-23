use crate::error::EngineError;
use pyo3::prelude::*;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE};
use reqwest::Client;

/// HTTP 探测结果
#[pyclass]
#[derive(Debug, Clone)]
pub struct ProbeResult {
    #[pyo3(get)]
    pub file_size: i64,
    #[pyo3(get)]
    pub supports_range: bool,
    #[pyo3(get)]
    pub file_name: String,
    #[pyo3(get)]
    pub final_url: String,
}

/// 探测 URL，获取文件大小、是否支持断点续传、文件名和最终 URL
pub async fn probe_url(
    client: &Client,
    url: &str,
    headers: &HeaderMap,
) -> Result<ProbeResult, EngineError> {
    // 发送带 Range: bytes=1-1 的 GET 请求探测服务器能力
    let mut req = client.get(url).headers(headers.clone());
    req = req.header("Range", "bytes=1-1");

    let resp = req.send().await?;
    let status = resp.status();
    let final_url = resp.url().to_string();
    let resp_headers = resp.headers().clone();

    let (file_size, supports_range) = if status.as_u16() == 206 {
        // 服务器支持 Range 请求
        let total = parse_range_total(&resp_headers).unwrap_or(-1);
        (total, true)
    } else if status.is_success() {
        // 服务器不支持 Range，使用 Content-Length
        let size = resp_headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(-1);
        (size, false)
    } else {
        // 请求失败，让 reqwest 处理错误
        let _ = resp.error_for_status()?;
        unreachable!()
    };

    // 提取文件名
    let file_name = extract_filename(&resp_headers, &final_url);

    Ok(ProbeResult {
        file_size,
        supports_range,
        file_name,
        final_url,
    })
}

/// 从 Content-Range 头解析文件总大小
/// 格式: bytes 1-1/12345 或 bytes */12345
fn parse_range_total(headers: &HeaderMap) -> Option<i64> {
    let value = headers.get(CONTENT_RANGE)?.to_str().ok()?;
    // Content-Range: bytes 1-1/12345
    let slash_pos = value.rfind('/')?;
    let total_str = &value[slash_pos + 1..];
    if total_str == "*" {
        return None;
    }
    total_str.trim().parse::<i64>().ok()
}

/// 从响应头或 URL 中提取文件名
fn extract_filename(headers: &HeaderMap, url: &str) -> String {
    // 优先从 Content-Disposition 头提取
    if let Some(cd) = headers.get(CONTENT_DISPOSITION) {
        if let Ok(cd_str) = cd.to_str() {
            if let Some(name) = parse_content_disposition(cd_str) {
                return name;
            }
        }
    }

    // 从 URL 路径提取
    filename_from_url(url)
}

/// 解析 Content-Disposition 头，支持 RFC 5987 filename*= 和普通 filename=
fn parse_content_disposition(header: &str) -> Option<String> {
    // 优先处理 filename*= (RFC 5987 编码)
    if let Some(name) = parse_filename_star(header) {
        return Some(name);
    }

    // 处理普通 filename=
    parse_filename_plain(header)
}

/// 解析 filename*= 参数 (RFC 5987)
/// 格式: filename*=UTF-8''%E6%96%87%E4%BB%B6.txt
fn parse_filename_star(header: &str) -> Option<String> {
    let lower = header.to_lowercase();
    let pos = lower.find("filename*=")?;
    let rest = &header[pos + "filename*=".len()..];

    // 找到值的结束位置（分号或字符串末尾）
    let value = rest.split(';').next()?.trim();

    // 格式: charset'language'encoded_value
    let parts: Vec<&str> = value.splitn(3, '\'').collect();
    if parts.len() != 3 {
        return None;
    }

    let encoded = parts[2].trim_matches('"');
    // 使用 percent-decoding 解码
    let decoded = urlencoding::decode(encoded).ok()?;
    let name = decoded.into_owned();

    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// 解析普通 filename= 参数
fn parse_filename_plain(header: &str) -> Option<String> {
    let lower = header.to_lowercase();
    let pos = lower.find("filename=")?;
    let rest = &header[pos + "filename=".len()..];

    let value = rest.split(';').next()?.trim();

    // 去除引号
    let name = value.trim_matches('"').trim().to_string();

    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// 从 URL 路径中提取文件名
fn filename_from_url(url: &str) -> String {
    // 去除查询参数和片段
    let path = url.split('?').next().unwrap_or(url);
    let path = path.split('#').next().unwrap_or(path);

    // 取最后一个路径段
    let name = path.rsplit('/').next().unwrap_or("download");

    // URL 解码
    let decoded = urlencoding::decode(name)
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| name.to_string());

    if decoded.is_empty() {
        "download".to_string()
    } else {
        decoded
    }
}

/// 构建 reqwest Client，支持代理和 SSL 设置
pub fn build_client(
    proxies: &std::collections::HashMap<String, String>,
    verify_ssl: bool,
    force_http1: bool,
) -> Result<Client, EngineError> {
    let mut builder = Client::builder()
        .user_agent("Ghost-Downloader/3 (gd3-engine)")
        .danger_accept_invalid_certs(!verify_ssl)
        .connect_timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(10));

    if force_http1 {
        builder = builder.http1_only();
    }

    // 配置代理
    for (_protocol, proxy_url) in proxies {
        let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| {
            EngineError::Http(e)
        })?;
        builder = builder.proxy(proxy);
    }

    let client = builder.build()?;
    Ok(client)
}

/// 将 HashMap<String, String> 转换为 reqwest HeaderMap
pub fn build_header_map(
    headers: &std::collections::HashMap<String, String>,
) -> Result<HeaderMap, EngineError> {
    let mut header_map = HeaderMap::new();
    for (key, value) in headers {
        let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
            .map_err(|e| EngineError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())))?;
        let val = HeaderValue::from_str(value)
            .map_err(|e| EngineError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())))?;
        header_map.insert(name, val);
    }
    Ok(header_map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_range_total() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_RANGE, HeaderValue::from_static("bytes 1-1/12345"));
        assert_eq!(parse_range_total(&headers), Some(12345));

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_RANGE, HeaderValue::from_static("bytes 0-999/1000"));
        assert_eq!(parse_range_total(&headers), Some(1000));

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_RANGE, HeaderValue::from_static("bytes */*"));
        assert_eq!(parse_range_total(&headers), None);
    }

    #[test]
    fn test_parse_content_disposition_plain() {
        let result = parse_content_disposition("attachment; filename=\"test.zip\"");
        assert_eq!(result, Some("test.zip".to_string()));

        let result = parse_content_disposition("attachment; filename=test.zip");
        assert_eq!(result, Some("test.zip".to_string()));
    }

    #[test]
    fn test_parse_content_disposition_rfc5987() {
        let result =
            parse_content_disposition("attachment; filename*=UTF-8''%E6%96%87%E4%BB%B6.txt");
        assert_eq!(result, Some("\u{6587}\u{4ef6}.txt".to_string()));
    }

    #[test]
    fn test_parse_content_disposition_star_priority() {
        // filename* 优先于 filename
        let result = parse_content_disposition(
            "attachment; filename=\"fallback.txt\"; filename*=UTF-8''%E6%96%87%E4%BB%B6.txt",
        );
        assert_eq!(result, Some("\u{6587}\u{4ef6}.txt".to_string()));
    }

    #[test]
    fn test_filename_from_url() {
        assert_eq!(
            filename_from_url("https://example.com/path/to/file.zip"),
            "file.zip"
        );
        assert_eq!(
            filename_from_url("https://example.com/path/to/file.zip?token=abc"),
            "file.zip"
        );
        assert_eq!(
            filename_from_url("https://example.com/path/to/%E6%96%87%E4%BB%B6.zip"),
            "\u{6587}\u{4ef6}.zip"
        );
        assert_eq!(filename_from_url("https://example.com/"), "download");
    }
}
