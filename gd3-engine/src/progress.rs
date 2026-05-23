use pyo3::prelude::*;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub struct ProgressInner {
    pub received_bytes: AtomicU64,
    pub total_bytes: AtomicI64,
    pub speed: AtomicU64,
    pub connections: AtomicU64,
    pub state: AtomicU64, // 0=downloading, 1=paused, 2=completed, 3=failed
    pub start_time: Instant,
}

impl ProgressInner {
    pub fn new(total_bytes: i64) -> Self {
        Self {
            received_bytes: AtomicU64::new(0),
            total_bytes: AtomicI64::new(total_bytes),
            speed: AtomicU64::new(0),
            connections: AtomicU64::new(0),
            state: AtomicU64::new(0),
            start_time: Instant::now(),
        }
    }
}

#[pyclass]
#[derive(Clone)]
pub struct DownloadProgress {
    inner: Arc<ProgressInner>,
    error: Arc<std::sync::Mutex<Option<String>>>,
}

impl DownloadProgress {
    pub fn new(inner: Arc<ProgressInner>) -> Self {
        Self {
            inner,
            error: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn set_error(&self, msg: String) {
        *self.error.lock().unwrap() = Some(msg);
    }

    pub fn inner(&self) -> &Arc<ProgressInner> {
        &self.inner
    }

    pub fn get_error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
}

#[pymethods]
impl DownloadProgress {
    #[getter]
    fn received_bytes(&self) -> u64 {
        self.inner.received_bytes.load(Ordering::Relaxed)
    }

    #[getter]
    fn total_bytes(&self) -> i64 {
        self.inner.total_bytes.load(Ordering::Relaxed)
    }

    #[getter]
    fn speed(&self) -> u64 {
        self.inner.speed.load(Ordering::Relaxed)
    }

    #[getter]
    fn connections(&self) -> u64 {
        self.inner.connections.load(Ordering::Relaxed)
    }

    #[getter]
    fn percent(&self) -> f64 {
        let total = self.inner.total_bytes.load(Ordering::Relaxed);
        if total <= 0 {
            return 0.0;
        }
        let received = self.inner.received_bytes.load(Ordering::Relaxed);
        (received as f64 / total as f64) * 100.0
    }

    #[getter]
    fn state(&self) -> &'static str {
        match self.inner.state.load(Ordering::Relaxed) {
            0 => "downloading",
            1 => "paused",
            2 => "completed",
            3 => "failed",
            _ => "unknown",
        }
    }

    #[getter]
    fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
}
