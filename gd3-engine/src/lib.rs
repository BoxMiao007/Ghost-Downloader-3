use pyo3::prelude::*;

mod config;
mod error;
mod progress;
pub mod resume;
pub mod speed_limit;
mod writer;

use config::DownloadConfig;
use progress::DownloadProgress;

#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[pymodule]
fn gd3_engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_class::<DownloadProgress>()?;
    m.add_class::<DownloadConfig>()?;
    Ok(())
}
