use crate::config::DownloadConfig;
use crate::connection::Connection;
use crate::error::EngineError;
use crate::progress::{DownloadProgress, ProgressInner};
use crate::resume::{Segment, read_ghdx, write_ghdx};
use crate::scheduler::{Scheduler, SchedulerConfig, SchedulerDecision};
use crate::speed_limit::SpeedLimiter;
use crate::writer::DiskWriter;

use pyo3::prelude::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use tokio::sync::Mutex as TokioMutex;
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;

/// 下载状态常量
#[allow(dead_code)]
const STATE_DOWNLOADING: u64 = 0;
const STATE_PAUSED: u64 = 1;
const STATE_COMPLETED: u64 = 2;
const STATE_FAILED: u64 = 3;

/// 监控循环间隔
const SUPERVISOR_INTERVAL_MS: u64 = 500;

/// 恢复文件保存间隔
const RESUME_SAVE_INTERVAL_MS: u64 = 2000;

/// Python 可见的下载句柄
#[pyclass]
pub struct DownloadHandle {
    /// 进度对象由下载线程和 Python UI 线程共享，只通过原子字段暴露可轮询状态。
    progress: DownloadProgress,
    cancel_token: CancellationToken,
    limiter: SpeedLimiter,
    /// 保持后台线程句柄存活，避免 DownloadHandle 还在 Python 侧使用时线程被提前丢弃。
    #[allow(dead_code)]
    join_handle: Option<thread::JoinHandle<()>>,
}

#[pymethods]
impl DownloadHandle {
    /// 获取下载进度对象
    #[getter]
    fn progress(&self) -> DownloadProgress {
        self.progress.clone()
    }

    /// 暂停下载
    fn pause(&self) {
        self.progress
            .inner()
            .state
            .store(STATE_PAUSED, Ordering::Relaxed);
        self.cancel_token.cancel();
    }

    /// 取消下载
    fn cancel(&self) {
        self.cancel_token.cancel();
    }

    /// 动态设置限速（字节/秒，0 表示不限速）
    fn set_speed_limit(&self, limit: u64) {
        self.limiter.set_limit(limit);
    }

    /// 阻塞等待下载完成（释放 GIL）
    fn wait_sync(&self, py: Python<'_>) -> PyResult<()> {
        // 释放 GIL 后轮询状态
        py.allow_threads(|| {
            loop {
                let state = self.progress.inner().state.load(Ordering::Relaxed);
                match state {
                    STATE_COMPLETED => return Ok(()),
                    STATE_FAILED => {
                        let err_msg = self
                            .progress
                            .get_error()
                            .unwrap_or_else(|| "download failed".to_string());
                        return Err(pyo3::exceptions::PyRuntimeError::new_err(err_msg));
                    }
                    _ => {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }
        })
    }
}

/// 启动下载，返回 DownloadHandle
pub fn start_download_inner(config: DownloadConfig) -> PyResult<DownloadHandle> {
    let file_size = config.file_size;
    let speed_limit = config.speed_limit;

    let progress_inner = Arc::new(ProgressInner::new(file_size));
    let progress = DownloadProgress::new(progress_inner.clone());
    let cancel_token = CancellationToken::new();
    let limiter = SpeedLimiter::new(speed_limit);

    let cancel_clone = cancel_token.clone();
    let limiter_clone = limiter.clone();
    let progress_clone = progress.clone();

    // PyO3 暴露的是同步 Python 对象，Tokio runtime 放到独立线程中运行，避免阻塞 Qt 事件循环。
    let join_handle = thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");

        rt.block_on(async {
            match run_download(config, progress_clone.clone(), cancel_clone, limiter_clone).await {
                Ok(()) => {
                    progress_clone
                        .inner()
                        .state
                        .store(STATE_COMPLETED, Ordering::Relaxed);
                }
                Err(EngineError::Cancelled) => {
                    // 暂停或取消，状态已在 pause() 中设置
                    let current = progress_clone.inner().state.load(Ordering::Relaxed);
                    if current != STATE_PAUSED {
                        // 如果不是暂停，标记为失败
                        progress_clone
                            .inner()
                            .state
                            .store(STATE_FAILED, Ordering::Relaxed);
                        progress_clone.set_error("Download cancelled".to_string());
                    }
                }
                Err(e) => {
                    progress_clone
                        .inner()
                        .state
                        .store(STATE_FAILED, Ordering::Relaxed);
                    progress_clone.set_error(e.to_string());
                }
            }
        });
    });

    Ok(DownloadHandle {
        progress,
        cancel_token,
        limiter,
        join_handle: Some(join_handle),
    })
}

/// 核心下载逻辑
async fn run_download(
    config: DownloadConfig,
    progress: DownloadProgress,
    cancel: CancellationToken,
    limiter: SpeedLimiter,
) -> Result<(), EngineError> {
    let file_size = config.file_size;
    let output_path = Path::new(&config.output_path);

    // DiskWriter 会在已知大小时预分配文件；未知大小或不支持 Range 的任务仍按流式写入处理。
    let writer = Arc::new(DiskWriter::open(output_path, file_size)?);

    // 全局已接收字节计数器
    let global_received = Arc::new(AtomicU64::new(0));

    // Python 侧会显式传入 .ghdx；兜底路径用于直接调用扩展 API 的场景。
    let resume_path = config
        .resume_file
        .clone()
        .unwrap_or_else(|| format!("{}.ghdx", config.output_path));

    // 加载或创建初始分片
    let initial_segments = load_or_create_segments(&config, &resume_path)?;

    // 恢复文件中的 downloaded 是绝对偏移，转换成已接收字节数后同步给 Python 进度对象。
    let already_downloaded: u64 = initial_segments
        .iter()
        .map(|s| s.downloaded - s.start)
        .sum();
    global_received.store(already_downloaded, Ordering::Relaxed);
    progress
        .inner()
        .received_bytes
        .store(already_downloaded, Ordering::Relaxed);

    // 连接任务会在完成或失败时回写 Segment；监控循环定期读取它来保存断点和做调度决策。
    let segments = Arc::new(TokioMutex::new(initial_segments));

    // 活跃连接计数
    let active_count = Arc::new(AtomicU64::new(0));
    progress.inner().connections.store(0, Ordering::Relaxed);

    // 启动初始连接任务
    let pending_segments: Vec<Segment> = {
        let segs = segments.lock().await;
        segs.iter()
            .filter(|s| s.status == 0 || s.status == 1)
            .cloned()
            .collect()
    };

    let task_set = Arc::new(TokioMutex::new(Vec::new()));

    for seg in pending_segments {
        spawn_connection(
            seg,
            &config.url,
            &config.headers,
            &config.proxies,
            config.verify_ssl,
            writer.clone(),
            &limiter,
            &cancel,
            global_received.clone(),
            segments.clone(),
            active_count.clone(),
            task_set.clone(),
        )
        .await;
    }

    // 监控循环
    supervisor_loop(
        &config,
        &progress,
        &cancel,
        &limiter,
        global_received.clone(),
        segments.clone(),
        active_count.clone(),
        task_set.clone(),
        writer.clone(),
        &resume_path,
    )
    .await?;

    // 最终同步磁盘
    writer.sync()?;

    // 只有完整成功后删除恢复文件；暂停/取消路径会保留它供下次续传。
    let _ = std::fs::remove_file(&resume_path);

    Ok(())
}

/// 加载恢复文件或创建新的分片分配
fn load_or_create_segments(
    config: &DownloadConfig,
    resume_path: &str,
) -> Result<Vec<Segment>, EngineError> {
    let path = Path::new(resume_path);

    // 只读取新版 .ghdx。旧版 .ghd 由 Python Worker 维护，避免两套引擎误用不同格式的断点文件。
    if path.exists() {
        if let Ok((_file_size, segments)) = read_ghdx(path) {
            return Ok(segments);
        }
    }

    // 创建新的分片分配。未知大小无法计算 Range 边界，只能交给单连接从头下载。
    let file_size = if config.file_size > 0 {
        config.file_size as u64
    } else {
        // 未知大小，单连接下载
        return Ok(vec![Segment {
            id: 0,
            start: 0,
            downloaded: 0,
            end: 0,
            status: 0,
            retries: 0,
        }]);
    };

    let sched_config = SchedulerConfig {
        file_size,
        supports_range: config.supports_range,
        // 当前 Python 入口尚未把探测速率传进来，调度器按保守默认值选择初始连接数。
        probe_throughput: 0,
        max_connections: config.max_connections as usize,
    };
    let mut scheduler = Scheduler::new(sched_config);
    let allocs = scheduler.initial_allocation();

    let segments = allocs
        .into_iter()
        .map(|a| Segment {
            id: a.id,
            start: a.start,
            downloaded: a.start,
            end: a.end,
            status: 0,
            retries: 0,
        })
        .collect();

    Ok(segments)
}

/// 启动一个连接任务
async fn spawn_connection(
    segment: Segment,
    url: &str,
    headers: &HashMap<String, String>,
    proxies: &HashMap<String, String>,
    verify_ssl: bool,
    writer: Arc<DiskWriter>,
    limiter: &SpeedLimiter,
    cancel: &CancellationToken,
    global_received: Arc<AtomicU64>,
    segments: Arc<TokioMutex<Vec<Segment>>>,
    active_count: Arc<AtomicU64>,
    task_set: Arc<TokioMutex<Vec<tokio::task::JoinHandle<Result<u32, EngineError>>>>>,
) {
    let url = url.to_string();
    let headers = headers.clone();
    let proxies = proxies.clone();
    let limiter = limiter.clone();
    let cancel = cancel.clone();
    let seg_id = segment.id;

    active_count.fetch_add(1, Ordering::Relaxed);

    let handle = tokio::spawn(async move {
        let mut conn = Connection::new(seg_id, segment);
        let limiter_opt = Some(limiter);

        let result = conn
            .run(
                &url,
                &headers,
                &proxies,
                verify_ssl,
                &writer,
                &limiter_opt,
                &cancel,
                &global_received,
            )
            .await;

        // 连接内部持有最新 downloaded/retries/status，退出时回写共享表供断点保存。
        {
            let mut segs = segments.lock().await;
            if let Some(s) = segs.iter_mut().find(|s| s.id == seg_id) {
                *s = conn.segment.clone();
            } else {
                segs.push(conn.segment.clone());
            }
        }

        active_count.fetch_sub(1, Ordering::Relaxed);

        match result {
            Ok(()) => Ok(seg_id),
            Err(e) => Err(e),
        }
    });

    task_set.lock().await.push(handle);
}

/// 监控循环：更新进度、保存恢复文件、调度决策
async fn supervisor_loop(
    config: &DownloadConfig,
    progress: &DownloadProgress,
    cancel: &CancellationToken,
    limiter: &SpeedLimiter,
    global_received: Arc<AtomicU64>,
    segments: Arc<TokioMutex<Vec<Segment>>>,
    active_count: Arc<AtomicU64>,
    task_set: Arc<TokioMutex<Vec<tokio::task::JoinHandle<Result<u32, EngineError>>>>>,
    writer: Arc<DiskWriter>,
    resume_path: &str,
) -> Result<(), EngineError> {
    let mut last_received: u64 = global_received.load(Ordering::Relaxed);
    let mut last_resume_save = tokio::time::Instant::now();
    let file_size = if config.file_size > 0 {
        config.file_size as u64
    } else {
        0
    };

    // 调度器（仅在支持 Range 且文件大小已知时启用）
    let mut scheduler = if config.supports_range && file_size > 0 {
        Some(Scheduler::new(SchedulerConfig {
            file_size,
            supports_range: true,
            probe_throughput: 0,
            max_connections: config.max_connections as usize,
        }))
    } else {
        None
    };

    loop {
        sleep(Duration::from_millis(SUPERVISOR_INTERVAL_MS)).await;

        // 取消时先等待连接任务退出，再写断点文件；否则可能保存到落后于磁盘写入的进度。
        if cancel.is_cancelled() {
            drain_tasks(&task_set).await;
            save_resume(&segments, file_size, resume_path).await;
            return Err(EngineError::Cancelled);
        }

        // 更新进度
        let current_received = global_received.load(Ordering::Relaxed);
        let speed = ((current_received - last_received) as f64
            / (SUPERVISOR_INTERVAL_MS as f64 / 1000.0)) as u64;
        progress.inner().speed.store(speed, Ordering::Relaxed);
        progress
            .inner()
            .received_bytes
            .store(current_received, Ordering::Relaxed);
        let active = active_count.load(Ordering::Relaxed);
        progress
            .inner()
            .connections
            .store(active, Ordering::Relaxed);
        last_received = current_received;

        // 检查是否完成
        if file_size > 0 && current_received >= file_size {
            drain_tasks(&task_set).await;
            return Ok(());
        }

        // 检查所有任务是否已结束（未知大小场景）
        if active == 0 {
            // 清理已完成的任务，检查错误
            let error = collect_task_errors(&task_set).await;
            if let Some(e) = error {
                return Err(e);
            }
            // 所有连接完成且无错误
            return Ok(());
        }

        // 定期保存恢复文件
        if last_resume_save.elapsed() >= Duration::from_millis(RESUME_SAVE_INTERVAL_MS) {
            save_resume(&segments, file_size, resume_path).await;
            last_resume_save = tokio::time::Instant::now();
        }

        // 调度决策
        if let Some(ref mut sched) = scheduler {
            let segs = segments.lock().await;
            let active_segs: Vec<Segment> =
                segs.iter().filter(|s| s.status == 1).cloned().collect();
            let decision = sched.evaluate(speed, &active_segs, active as usize);
            drop(segs);

            match decision {
                SchedulerDecision::Split(alloc) => {
                    // 新连接接管后半段，原连接只需缩短 end，避免两个连接继续下载同一段。
                    {
                        let mut segs = segments.lock().await;
                        if let Some(parent) = segs
                            .iter_mut()
                            .find(|s| s.end == alloc.end && s.status == 1 && s.id != alloc.id)
                        {
                            parent.end = alloc.start;
                        }
                    }
                    let new_seg = Segment {
                        id: alloc.id,
                        start: alloc.start,
                        downloaded: alloc.start,
                        end: alloc.end,
                        status: 0,
                        retries: 0,
                    };
                    spawn_connection(
                        new_seg,
                        &config.url,
                        &config.headers,
                        &config.proxies,
                        config.verify_ssl,
                        writer.clone(),
                        limiter,
                        cancel,
                        global_received.clone(),
                        segments.clone(),
                        active_count.clone(),
                        task_set.clone(),
                    )
                    .await;
                }
                SchedulerDecision::MarkSlowest(_id) => {
                    // 调度器已记录该分片 ID；这里不取消连接，只阻止后续继续拆慢分片。
                }
                SchedulerDecision::NoOp => {}
            }
        }

        // 清理已完成的 JoinHandle
        cleanup_finished_tasks(&task_set).await;
    }
}

/// 等待所有任务完成
async fn drain_tasks(
    task_set: &Arc<TokioMutex<Vec<tokio::task::JoinHandle<Result<u32, EngineError>>>>>,
) {
    let mut tasks = task_set.lock().await;
    for handle in tasks.drain(..) {
        let _ = handle.await;
    }
}

/// 收集任务错误
async fn collect_task_errors(
    task_set: &Arc<TokioMutex<Vec<tokio::task::JoinHandle<Result<u32, EngineError>>>>>,
) -> Option<EngineError> {
    let mut tasks = task_set.lock().await;
    for handle in tasks.drain(..) {
        if let Ok(Err(e)) = handle.await {
            if !matches!(e, EngineError::Cancelled) {
                return Some(e);
            }
        }
    }
    None
}

/// 清理已完成的任务句柄
async fn cleanup_finished_tasks(
    task_set: &Arc<TokioMutex<Vec<tokio::task::JoinHandle<Result<u32, EngineError>>>>>,
) {
    let mut tasks = task_set.lock().await;
    let mut i = 0;
    while i < tasks.len() {
        if tasks[i].is_finished() {
            tasks.swap_remove(i);
        } else {
            i += 1;
        }
    }
}

/// 保存恢复文件
async fn save_resume(segments: &Arc<TokioMutex<Vec<Segment>>>, file_size: u64, resume_path: &str) {
    let segs = segments.lock().await;
    let _ = write_ghdx(Path::new(resume_path), file_size, &segs);
}
