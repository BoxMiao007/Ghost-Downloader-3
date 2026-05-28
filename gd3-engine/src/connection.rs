use crate::error::EngineError;
use crate::probe::{build_client, build_header_map};
use crate::resume::Segment;
use crate::speed_limit::SpeedLimiter;
use crate::writer::DiskWriter;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;

/// 单分片下载连接
pub struct Connection {
    pub id: u32,
    /// Segment 保存当前分片的边界和下一次写入偏移，退出时会回写到共享状态。
    pub segment: Segment,
    pub bytes_downloaded: Arc<AtomicU64>,
    /// 某些服务器在 HTTP/2 分片请求下返回 5xx，重试时降级到 HTTP/1.1 提高兼容性。
    pub force_http1: bool,
}

/// 分片状态常量
const STATUS_PENDING: u8 = 0;
const STATUS_RUNNING: u8 = 1;
const STATUS_COMPLETED: u8 = 2;
const STATUS_FAILED: u8 = 3;

/// 最大重试次数
const MAX_RETRIES: u8 = 5;

/// 退避时间序列（秒）
const BACKOFF_SECS: [u64; 5] = [1, 2, 4, 8, 16];

/// 最大退避时间（秒）
const MAX_BACKOFF_SECS: u64 = 30;

impl Connection {
    pub fn new(id: u32, segment: Segment) -> Self {
        Self {
            id,
            segment,
            bytes_downloaded: Arc::new(AtomicU64::new(0)),
            force_http1: false,
        }
    }

    /// 执行下载，带重试、指数退避和 HTTP 版本降级
    pub async fn run(
        &mut self,
        url: &str,
        headers: &HashMap<String, String>,
        proxies: &HashMap<String, String>,
        verify_ssl: bool,
        writer: &DiskWriter,
        limiter: &Option<SpeedLimiter>,
        cancel: &CancellationToken,
        global_received: &Arc<AtomicU64>,
    ) -> Result<(), EngineError> {
        self.segment.status = STATUS_RUNNING;
        let mut last_error: Option<EngineError> = None;

        for attempt in 0..MAX_RETRIES {
            if cancel.is_cancelled() {
                self.segment.status = STATUS_PENDING;
                return Err(EngineError::Cancelled);
            }

            match self
                .download_range(
                    url,
                    headers,
                    proxies,
                    verify_ssl,
                    writer,
                    limiter,
                    cancel,
                    global_received,
                )
                .await
            {
                Ok(()) => {
                    self.segment.status = STATUS_COMPLETED;
                    return Ok(());
                }
                Err(EngineError::Cancelled) => {
                    self.segment.status = STATUS_PENDING;
                    return Err(EngineError::Cancelled);
                }
                Err(e) => {
                    // 仅服务端错误触发 HTTP/1.1 降级，网络错误继续按原协议重试。
                    if is_server_error(&e) && !self.force_http1 {
                        self.force_http1 = true;
                    }

                    last_error = Some(e);
                    self.segment.retries = attempt + 1;

                    if attempt + 1 < MAX_RETRIES {
                        let backoff = BACKOFF_SECS
                            .get(attempt as usize)
                            .copied()
                            .unwrap_or(MAX_BACKOFF_SECS)
                            .min(MAX_BACKOFF_SECS);

                        tokio::select! {
                            _ = sleep(Duration::from_secs(backoff)) => {}
                            _ = cancel.cancelled() => {
                                self.segment.status = STATUS_PENDING;
                                return Err(EngineError::Cancelled);
                            }
                        }
                    }
                }
            }
        }

        self.segment.status = STATUS_FAILED;
        Err(EngineError::MaxRetries {
            id: self.id,
            reason: last_error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown error".to_string()),
        })
    }

    /// 下载指定范围的数据
    async fn download_range(
        &mut self,
        url: &str,
        headers: &HashMap<String, String>,
        proxies: &HashMap<String, String>,
        verify_ssl: bool,
        writer: &DiskWriter,
        limiter: &Option<SpeedLimiter>,
        cancel: &CancellationToken,
        global_received: &Arc<AtomicU64>,
    ) -> Result<(), EngineError> {
        let client = build_client(proxies, verify_ssl, self.force_http1)?;
        let header_map = build_header_map(headers)?;

        let range_header = format!("bytes={}-{}", self.segment.downloaded, self.segment.end);

        let resp = client
            .get(url)
            .headers(header_map)
            .header("Range", &range_header)
            .send()
            .await?;

        // 检查响应状态
        let resp = resp.error_for_status()?;

        // 流式读取响应体
        let mut stream = resp.bytes_stream();
        let mut write_offset = self.segment.downloaded;

        while let Some(chunk_result) = stream.next().await {
            // 检查取消
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }

            let chunk = chunk_result?;
            let chunk_len = chunk.len() as u64;

            // 限速
            if let Some(ref lim) = limiter {
                lim.acquire(chunk_len).await;
            }

            // 使用 pwrite 按绝对偏移写入，避免并发分片共享文件游标导致错位。
            writer.pwrite(&chunk, write_offset)?;

            // 计数器允许监控循环无锁读取速度；精确分片状态仍在任务结束时统一回写。
            write_offset += chunk_len;
            self.segment.downloaded += chunk_len;
            self.bytes_downloaded
                .fetch_add(chunk_len, Ordering::Relaxed);
            global_received.fetch_add(chunk_len, Ordering::Relaxed);
        }

        Ok(())
    }
}

/// 判断错误是否为 5xx 服务端错误（触发 HTTP 版本降级）
fn is_server_error(err: &EngineError) -> bool {
    if let EngineError::Http(e) = err {
        if let Some(status) = e.status() {
            return status.is_server_error();
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_new() {
        let segment = Segment {
            id: 0,
            start: 0,
            downloaded: 0,
            end: 9999,
            status: 0,
            retries: 0,
        };
        let conn = Connection::new(0, segment);
        assert_eq!(conn.id, 0);
        assert_eq!(conn.segment.start, 0);
        assert_eq!(conn.segment.end, 9999);
        assert_eq!(conn.bytes_downloaded.load(Ordering::Relaxed), 0);
    }
}
