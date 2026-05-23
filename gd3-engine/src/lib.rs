use pyo3::prelude::*;
use std::collections::HashMap;

mod config;
mod connection;
mod error;
pub mod probe;
mod progress;
pub mod resume;
pub mod speed_limit;
mod writer;

use config::DownloadConfig;
use probe::ProbeResult;
use progress::DownloadProgress;

#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 探测 URL，获取文件信息和服务器能力
#[pyfunction]
#[pyo3(signature = (url, headers=HashMap::new(), proxies=HashMap::new(), verify_ssl=false))]
fn probe_url(
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

#[pymodule]
fn gd3_engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(probe_url, m)?)?;
    m.add_class::<DownloadProgress>()?;
    m.add_class::<DownloadConfig>()?;
    m.add_class::<ProbeResult>()?;
    Ok(())
}
