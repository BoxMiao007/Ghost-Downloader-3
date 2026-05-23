use crate::error::EngineError;
use std::fs::{File, OpenOptions};
use std::path::Path;

/// 磁盘写入器，支持跨平台定位写入
pub struct DiskWriter {
    file: File,
}

impl DiskWriter {
    pub fn open(path: &Path, file_size: i64) -> Result<Self, EngineError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        if file_size > 0 {
            file.set_len(file_size as u64)?;
        }

        Ok(Self { file })
    }

    /// 在指定偏移量处写入数据（无需 seek，线程安全）
    #[cfg(unix)]
    pub fn pwrite(&self, buf: &[u8], offset: u64) -> Result<(), EngineError> {
        use std::os::unix::fs::FileExt;
        self.file.write_all_at(buf, offset)?;
        Ok(())
    }

    /// 在指定偏移量处写入数据（Windows 版本）
    #[cfg(windows)]
    pub fn pwrite(&self, buf: &[u8], offset: u64) -> Result<(), EngineError> {
        use std::os::windows::fs::FileExt;
        self.file.seek_write(buf, offset)?;
        Ok(())
    }

    /// 将文件数据同步到磁盘
    pub fn sync(&self) -> Result<(), EngineError> {
        self.file.sync_data()?;
        Ok(())
    }
}
