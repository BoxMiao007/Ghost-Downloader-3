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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::tempdir;

    #[test]
    fn test_open_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sub").join("dir").join("file.bin");
        let _writer = DiskWriter::open(&path, 100).unwrap();
        assert!(path.exists());
        // 文件应预分配为 100 字节
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 100);
    }

    #[test]
    fn test_pwrite_at_offset() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let writer = DiskWriter::open(&path, 20).unwrap();

        writer.pwrite(b"hello", 0).unwrap();
        writer.pwrite(b"world", 10).unwrap();

        let mut buf = vec![0u8; 20];
        let mut f = std::fs::File::open(&path).unwrap();
        f.read_exact(&mut buf).unwrap();

        assert_eq!(&buf[0..5], b"hello");
        assert_eq!(&buf[5..10], &[0, 0, 0, 0, 0]); // 间隙为零
        assert_eq!(&buf[10..15], b"world");
    }

    #[test]
    fn test_pwrite_concurrent_offsets() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("concurrent.bin");
        let writer = DiskWriter::open(&path, 30).unwrap();

        // 模拟不同偏移量的并发写入
        writer.pwrite(b"AAAAAAAAAA", 0).unwrap();
        writer.pwrite(b"BBBBBBBBBB", 10).unwrap();
        writer.pwrite(b"CCCCCCCCCC", 20).unwrap();

        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[0..10], b"AAAAAAAAAA");
        assert_eq!(&data[10..20], b"BBBBBBBBBB");
        assert_eq!(&data[20..30], b"CCCCCCCCCC");
    }

    #[test]
    fn test_open_zero_size_no_truncate() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("zero.bin");
        // file_size <= 0 表示不预分配
        let _writer = DiskWriter::open(&path, 0).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    }
}
