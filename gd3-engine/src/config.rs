use pyo3::prelude::*;
use std::collections::HashMap;

#[pyclass]
#[derive(Clone, Debug)]
pub struct DownloadConfig {
    #[pyo3(get, set)]
    pub url: String,
    #[pyo3(get, set)]
    pub output_path: String,
    #[pyo3(get, set)]
    pub headers: HashMap<String, String>,
    #[pyo3(get, set)]
    pub proxies: HashMap<String, String>,
    #[pyo3(get, set)]
    pub file_size: i64,
    #[pyo3(get, set)]
    pub supports_range: bool,
    #[pyo3(get, set)]
    pub speed_limit: u64,
    #[pyo3(get, set)]
    pub max_connections: u32,
    #[pyo3(get, set)]
    pub verify_ssl: bool,
    #[pyo3(get, set)]
    pub resume_file: Option<String>,
}

#[pymethods]
impl DownloadConfig {
    #[new]
    #[pyo3(signature = (url, output_path, headers=HashMap::new(), proxies=HashMap::new(), file_size=-1, supports_range=true, speed_limit=0, max_connections=64, verify_ssl=false, resume_file=None))]
    fn new(
        url: String,
        output_path: String,
        headers: HashMap<String, String>,
        proxies: HashMap<String, String>,
        file_size: i64,
        supports_range: bool,
        speed_limit: u64,
        max_connections: u32,
        verify_ssl: bool,
        resume_file: Option<String>,
    ) -> Self {
        Self {
            url,
            output_path,
            headers,
            proxies,
            file_size,
            supports_range,
            speed_limit,
            max_connections,
            verify_ssl,
            resume_file,
        }
    }
}
