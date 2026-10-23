use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static WRITE_ATOMIC_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn nproc() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

pub fn default_build_jobs(max_build_memory_frac: f64) -> usize {
    let by_cpu = (nproc() / 8).clamp(1, 16);
    let by_ram = ram_budget_jobs(max_build_memory_frac).max(1);
    by_cpu.min(by_ram).max(1)
}

pub fn ram_budget_jobs(frac: f64) -> usize {
    let total = mem_total_bytes().unwrap_or(16 * (1 << 30));
    let budget = (total as f64 * frac) as u64;
    // Rough per-rustc working set. Too many concurrent rustcs evict the page cache.
    const PER_JOB: u64 = 2 * (1 << 30);
    (budget / PER_JOB).max(1) as usize
}

pub fn mem_total_bytes() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

pub fn sha256_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finalize())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(sha256_bytes(&data))
}

pub fn rel_slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub fn which(name: &str) -> Option<PathBuf> {
    let Ok(path) = std::env::var("PATH") else {
        return None;
    };
    for dir in path.split(':') {
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn format_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "n/a".into();
    }
    if secs < 1.0 {
        return format!("{:.0} ms", secs * 1000.0);
    }
    if secs < 60.0 {
        return format!("{secs:.1} s");
    }
    if secs < 3600.0 {
        return format!("{:.1} min", secs / 60.0);
    }
    if secs < 86400.0 {
        return format!("{:.1} h", secs / 3600.0);
    }
    format!("{:.1} d", secs / 86400.0)
}

pub fn duration_from_secs(secs: f64) -> Duration {
    Duration::from_secs_f64(secs.max(0.0))
}

pub fn write_atomic(path: &Path, data: impl AsRef<[u8]>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Unique tmp per process. Parallel estimate shards share an index dir;
    // `foo.json` → `foo.tmp` raced: one rename unlinked the other's tmp and
    // the next rename returned ENOENT.
    let tmp = unique_tmp_path(path);
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(data.as_ref())
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("sync {}", tmp.display()))?;
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

pub fn unique_tmp_path(path: &Path) -> PathBuf {
    let seq = WRITE_ATOMIC_SEQ.fetch_add(1, Ordering::Relaxed);
    match path.file_name() {
        Some(name) => path.with_file_name(format!(
            ".{}.{}.{seq}.tmp",
            name.to_string_lossy(),
            std::process::id()
        )),
        None => path.with_extension(format!("{}.{seq}.tmp", std::process::id())),
    }
}

pub fn statvfs_avail(path: &Path) -> Result<u64> {
    let c = std::ffi::CString::new(path.to_string_lossy().as_bytes())
        .context("path contains interior NUL")?;
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) != 0 {
            return Err(std::io::Error::last_os_error()).context("statvfs");
        }
        Ok(s.f_bavail as u64 * s.f_frsize as u64)
    }
}

/// Whether `path` is on an NFS mount (`statfs` magic 0x6969).
pub fn on_nfs(path: &Path) -> bool {
    const NFS_SUPER_MAGIC: i64 = 0x6969;
    let Ok(c) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) else {
        return false;
    };
    unsafe {
        let mut s: libc::statfs = std::mem::zeroed();
        if libc::statfs(c.as_ptr(), &mut s) != 0 {
            return false;
        }
        s.f_type as i64 == NFS_SUPER_MAGIC
    }
}

pub fn fill_incompressible(len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let mut x: u64 = 0xC0FFEE_u64.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for chunk in buf.chunks_mut(8) {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        let bytes = x.to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    buf
}

/// Convert a proc-macro2 `LineColumn` to a byte offset.
///
/// `column` is a 0-based *character* index, not a byte index. Treating it as
/// bytes truncates the span on any multibyte character in the line (café, CJK).
pub fn line_col_to_offset(src: &str, line: usize, column: usize) -> usize {
    let mut off = 0usize;
    for (current_line, part) in (1usize..).zip(src.split_inclusive('\n')) {
        if current_line == line {
            return off
                + part
                    .chars()
                    .take(column)
                    .map(|c| c.len_utf8())
                    .sum::<usize>();
        }
        off += part.len();
    }
    src.len()
}

pub fn parse_memory_spec(s: &str) -> Result<f64> {
    let t = s.trim();
    if let Some(p) = t.strip_suffix('%') {
        let v: f64 = p.trim().parse().context("memory percent")?;
        return Ok((v / 100.0).clamp(0.05, 0.95));
    }
    Ok(0.70)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_treats_column_as_chars() {
        let src = "fn f() -> bool { let x = \"café\"; x.is_empty() }";
        // The closing brace is the last character.
        let close = src.rfind('}').unwrap();
        let col = src[..close + 1].chars().count(); // 0-based end column on line 1
        assert_eq!(line_col_to_offset(src, 1, col), close + 1);
        // Byte-as-column would undershoot by the extra byte in é.
        let naive = col.min(src.len());
        assert!(naive < src.len() || src.is_ascii());
        assert_eq!(src.as_bytes()[close], b'}');
    }

    #[test]
    fn line_col_cjk_does_not_truncate() {
        let src = "fn f() -> i32 { let x = \"日本語\"; 1 }";
        let close = src.rfind('}').unwrap();
        let col = src[..close + 1].chars().count();
        assert_eq!(line_col_to_offset(src, 1, col), close + 1);
        assert!(src[line_col_to_offset(src, 1, 0)..line_col_to_offset(src, 1, col)].ends_with('}'));
    }

    #[test]
    fn write_atomic_survives_parallel_writers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cargo-metadata.json");
        std::thread::scope(|scope| {
            for i in 0..8u8 {
                let path = path.clone();
                scope.spawn(move || {
                    let body = vec![b'a' + i; 64];
                    write_atomic(&path, body).expect("parallel write_atomic");
                });
            }
        });
        assert!(path.is_file());
        assert_eq!(fs::read(&path).unwrap().len(), 64);
    }
}
