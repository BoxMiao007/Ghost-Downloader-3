use crate::error::EngineError;
use crc32fast::Hasher;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 文件分片信息
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub id: u32,
    pub start: u64,
    pub downloaded: u64,
    pub end: u64,
    pub status: u8,
    pub retries: u8,
}

const GHDX_MAGIC: &[u8; 4] = b"GHDX";
const GHDX_VERSION: u16 = 1;
const GHDX_HEADER_SIZE: usize = 32;
const GHDX_SEGMENT_SIZE: usize = 32;
/// Python Worker 的旧断点格式没有状态和校验，只用于读取迁移，不再由 Rust 引擎写入。
const GHD_SEGMENT_SIZE: usize = 24; // 3 x u64 LE

/// 解析旧版 .ghd 二进制格式（24字节分片：3 x u64 LE = start, progress, end）
pub fn parse_ghd(path: &Path) -> Result<Vec<Segment>, EngineError> {
    let data = fs::read(path)?;

    if data.len() % GHD_SEGMENT_SIZE != 0 {
        return Err(EngineError::CorruptedResumeFile(format!(
            "GHD file size {} is not a multiple of {}",
            data.len(),
            GHD_SEGMENT_SIZE
        )));
    }

    let count = data.len() / GHD_SEGMENT_SIZE;
    let mut segments = Vec::with_capacity(count);

    for i in 0..count {
        let offset = i * GHD_SEGMENT_SIZE;
        let start = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        let downloaded = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
        let end = u64::from_le_bytes(data[offset + 16..offset + 24].try_into().unwrap());

        segments.push(Segment {
            id: i as u32,
            start,
            downloaded,
            end,
            status: 0,
            retries: 0,
        });
    }

    Ok(segments)
}

/// 写入新版 .ghdx 格式（原子写入：先写临时文件再重命名）
///
/// 头部 32 字节：
///   magic "GHDX" (4), version u16 (2), flags u16 (2),
///   file_size u64 (8), created_at u64 (8),
///   checksum u32 (4), reserved (4)
///
/// 每个分片 32 字节：
///   id u32 (4), start u64 (8), downloaded u64 (8),
///   end u64 (8), status u8 (1), retries u8 (1), reserved (2)
pub fn write_ghdx(path: &Path, file_size: u64, segments: &[Segment]) -> Result<(), EngineError> {
    let total_size = GHDX_HEADER_SIZE + segments.len() * GHDX_SEGMENT_SIZE;
    let mut buf = vec![0u8; total_size];

    // 写入头部
    buf[0..4].copy_from_slice(GHDX_MAGIC);
    buf[4..6].copy_from_slice(&GHDX_VERSION.to_le_bytes());
    buf[6..8].copy_from_slice(&0u16.to_le_bytes()); // flags
    buf[8..16].copy_from_slice(&file_size.to_le_bytes());

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    buf[16..24].copy_from_slice(&created_at.to_le_bytes());
    // checksum 字段 [24..28] 稍后填入
    // reserved [28..32] 保持为 0

    // 写入分片
    for (i, seg) in segments.iter().enumerate() {
        let offset = GHDX_HEADER_SIZE + i * GHDX_SEGMENT_SIZE;
        buf[offset..offset + 4].copy_from_slice(&seg.id.to_le_bytes());
        buf[offset + 4..offset + 12].copy_from_slice(&seg.start.to_le_bytes());
        buf[offset + 12..offset + 20].copy_from_slice(&seg.downloaded.to_le_bytes());
        buf[offset + 20..offset + 28].copy_from_slice(&seg.end.to_le_bytes());
        buf[offset + 28] = seg.status;
        buf[offset + 29] = seg.retries;
        // reserved [offset+30..offset+32] 保持为 0
    }

    // checksum 写入前按 0 参与计算，使读写两端可用同一套 compute_crc32 逻辑。
    let checksum = compute_crc32(&buf);
    buf[24..28].copy_from_slice(&checksum.to_le_bytes());

    // 断点文件可能被频繁刷新，临时文件 + rename 可避免崩溃时留下半截记录。
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(&buf)?;
    tmp.flush()?;
    tmp.persist(path).map_err(|e| {
        EngineError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        ))
    })?;

    Ok(())
}

/// 读取 .ghdx 文件并验证 CRC32
pub fn read_ghdx(path: &Path) -> Result<(u64, Vec<Segment>), EngineError> {
    let data = fs::read(path)?;

    if data.len() < GHDX_HEADER_SIZE {
        return Err(EngineError::CorruptedResumeFile(
            "GHDX file too small for header".to_string(),
        ));
    }

    // 验证 magic
    if &data[0..4] != GHDX_MAGIC {
        return Err(EngineError::CorruptedResumeFile(
            "Invalid GHDX magic bytes".to_string(),
        ));
    }

    // 先校验再解析分片，避免损坏文件中的随机偏移被当作有效续传进度。
    let stored_checksum = u32::from_le_bytes(data[24..28].try_into().unwrap());
    let computed_checksum = compute_crc32(&data);
    if stored_checksum != computed_checksum {
        return Err(EngineError::CorruptedResumeFile(format!(
            "CRC32 mismatch: stored={:#010x}, computed={:#010x}",
            stored_checksum, computed_checksum
        )));
    }

    let file_size = u64::from_le_bytes(data[8..16].try_into().unwrap());

    // 解析分片
    let segment_data = &data[GHDX_HEADER_SIZE..];
    if segment_data.len() % GHDX_SEGMENT_SIZE != 0 {
        return Err(EngineError::CorruptedResumeFile(
            "GHDX segment data size is not aligned".to_string(),
        ));
    }

    let count = segment_data.len() / GHDX_SEGMENT_SIZE;
    let mut segments = Vec::with_capacity(count);

    for i in 0..count {
        let offset = i * GHDX_SEGMENT_SIZE;
        let id = u32::from_le_bytes(segment_data[offset..offset + 4].try_into().unwrap());
        let start = u64::from_le_bytes(segment_data[offset + 4..offset + 12].try_into().unwrap());
        let downloaded =
            u64::from_le_bytes(segment_data[offset + 12..offset + 20].try_into().unwrap());
        let end = u64::from_le_bytes(segment_data[offset + 20..offset + 28].try_into().unwrap());
        let status = segment_data[offset + 28];
        let retries = segment_data[offset + 29];

        segments.push(Segment {
            id,
            start,
            downloaded,
            end,
            status,
            retries,
        });
    }

    Ok((file_size, segments))
}

/// 计算 CRC32，跳过 checksum 字段 [24..28]
fn compute_crc32(data: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(&data[..24]);
    hasher.update(&[0u8; 4]); // checksum 字段视为零
    if data.len() > 28 {
        hasher.update(&data[28..]);
    }
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_ghd_file() {
        let mut file = NamedTempFile::new().unwrap();
        let data: Vec<(u64, u64, u64)> = vec![
            (0, 1000, 9999),
            (10000, 15000, 19999),
            (20000, 20000, 29999),
        ];
        for (start, progress, end) in &data {
            file.write_all(&start.to_le_bytes()).unwrap();
            file.write_all(&progress.to_le_bytes()).unwrap();
            file.write_all(&end.to_le_bytes()).unwrap();
        }
        file.flush().unwrap();

        let segments = parse_ghd(file.path()).unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].start, 0);
        assert_eq!(segments[0].downloaded, 1000);
        assert_eq!(segments[0].end, 9999);
        assert_eq!(segments[1].start, 10000);
        assert_eq!(segments[1].downloaded, 15000);
        assert_eq!(segments[2].downloaded, 20000);
    }

    #[test]
    fn test_parse_ghd_empty_file() {
        let file = NamedTempFile::new().unwrap();
        let segments = parse_ghd(file.path()).unwrap();
        assert_eq!(segments.len(), 0);
    }

    #[test]
    fn test_ghdx_roundtrip() {
        let segments = vec![
            Segment {
                id: 0,
                start: 0,
                downloaded: 5000,
                end: 9999,
                status: 1,
                retries: 0,
            },
            Segment {
                id: 1,
                start: 10000,
                downloaded: 10000,
                end: 19999,
                status: 0,
                retries: 2,
            },
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.ghdx");
        write_ghdx(&path, 20000, &segments).unwrap();

        let (file_size, loaded) = read_ghdx(&path).unwrap();
        assert_eq!(file_size, 20000);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, 0);
        assert_eq!(loaded[0].downloaded, 5000);
        assert_eq!(loaded[1].retries, 2);
    }

    #[test]
    fn test_ghdx_corrupted_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.ghdx");
        let segments = vec![Segment {
            id: 0,
            start: 0,
            downloaded: 100,
            end: 999,
            status: 1,
            retries: 0,
        }];
        write_ghdx(&path, 1000, &segments).unwrap();

        // 篡改分片数据区域的一个字节
        let mut data = fs::read(&path).unwrap();
        data[40] ^= 0xFF;
        fs::write(&path, &data).unwrap();

        let result = read_ghdx(&path);
        assert!(result.is_err());
    }
}
