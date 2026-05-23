use pyo3::prelude::*;
use std::collections::HashMap;

mod config;
mod connection;
mod engine;
mod error;
pub mod probe;
mod progress;
pub mod resume;
mod scheduler;
pub mod speed_limit;
mod writer;

use config::DownloadConfig;
use engine::DownloadHandle;
use probe::ProbeResult;
use progress::DownloadProgress;

#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 探测 URL，获取文件信息和服务器能力
#[pyfunction]
#[pyo3(name = "probe", signature = (url, headers=HashMap::new(), proxies=HashMap::new(), verify_ssl=false))]
fn probe_url_py(
    url: String,
    headers: HashMap<String, String>,
    proxies: HashMap<String, String>,
    verify_ssl: bool,
) -> PyResult<ProbeResult> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

    rt.block_on(async {
        let client = probe::build_client(&proxies, verify_ssl)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        let header_map = probe::build_header_map(&headers)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        probe::probe_url(&client, &url, &header_map)
            .await
            .map_err(|e| e.into())
    })
}

/// 启动下载任务，返回 DownloadHandle
#[pyfunction]
fn start_download(config: DownloadConfig) -> PyResult<DownloadHandle> {
    engine::start_download_inner(config)
}

#[pymodule]
fn gd3_engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(probe_url_py, m)?)?;
    m.add_function(wrap_pyfunction!(start_download, m)?)?;
    m.add_class::<DownloadProgress>()?;
    m.add_class::<DownloadConfig>()?;
    m.add_class::<ProbeResult>()?;
    m.add_class::<DownloadHandle>()?;
    Ok(())
}
