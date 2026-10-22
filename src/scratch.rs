use crate::config::{CloneBackendKind, Config};
use crate::types::Paths;
use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

const PROBE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sharing {
    SharedBlocks,
    ServerSideCopy,
    FullCopy,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub backend: CloneBackendKind,
    pub sharing: Sharing,
    pub message: String,
    pub clone_secs: f64,
    pub space_delta: i64,
}

pub trait CloneBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn clone_file(&self, src: &Path, dst: &Path) -> Result<()>;
    fn clone_tree(&self, src: &Path, dst: &Path) -> Result<()>;
}

pub struct ReflinkBackend;
pub struct CopyBackend {
    pub server_side: bool,
}
pub struct ZfsCloneBackend {
    pub remote: Option<String>,
    pub dataset: Option<String>,
}

impl CloneBackend for ReflinkBackend {
    fn name(&self) -> &'static str {
        "reflink"
    }
    fn clone_file(&self, src: &Path, dst: &Path) -> Result<()> {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        reflink_copy::reflink(src, dst).with_context(|| {
            format!(
                "FICLONE {} -> {}\n{}",
                src.display(),
                dst.display(),
                FICLONE_CHAIN
            )
        })?;
        keep_times(src, dst)?;
        Ok(())
    }
    fn clone_tree(&self, src: &Path, dst: &Path) -> Result<()> {
        clone_tree_with(self, src, dst)
    }
}

impl CloneBackend for CopyBackend {
    fn name(&self) -> &'static str {
        if self.server_side {
            "copy (server-side copy_file_range)"
        } else {
            "copy"
        }
    }
    fn clone_file(&self, src: &Path, dst: &Path) -> Result<()> {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        if copy_file_range_path(src, dst).is_ok() {
            return Ok(());
        }
        fs::copy(src, dst)
            .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
        keep_times(src, dst)?;
        Ok(())
    }
    fn clone_tree(&self, src: &Path, dst: &Path) -> Result<()> {
        clone_tree_with(self, src, dst)
    }
}

impl CloneBackend for ZfsCloneBackend {
    fn name(&self) -> &'static str {
        "zfs-clone"
    }
    fn clone_file(&self, src: &Path, dst: &Path) -> Result<()> {
        // File-level fallback inside a cloned dataset still uses reflink when possible.
        if reflink_copy::reflink(src, dst).is_err() {
            fs::copy(src, dst).context("zfs-clone file copy")?;
        }
        keep_times(src, dst)?;
        Ok(())
    }
    fn clone_tree(&self, src: &Path, dst: &Path) -> Result<()> {
        zfs_clone_tree(self, src, dst)
    }
}

const FICLONE_CHAIN: &str = "\
For this scratch root, the clone chain is:
  client FICLONE → NFSv4.2 CLONE → nfsd → zfs remap_file_range → block clone

Every link must hold. OpenZFS block cloning is version-gated and often
disabled (zfs_bclone_enabled=0). Check that:
  - the NFS export is v4.2
  - the server filesystem is ZFS with block cloning enabled
  - source and destination sit on the same exported dataset";

pub fn probe_and_select(cfg: &Config) -> Result<(Arc<dyn CloneBackend>, ProbeReport)> {
    fs::create_dir_all(&cfg.scratch_root)
        .with_context(|| format!("create scratch root {}", cfg.scratch_root.display()))?;
    let report = probe(&cfg.scratch_root, cfg.clone_backend, cfg.allow_copy)?;
    let backend: Arc<dyn CloneBackend> = match report.backend {
        CloneBackendKind::Reflink => Arc::new(ReflinkBackend),
        CloneBackendKind::Copy => Arc::new(CopyBackend {
            server_side: report.sharing == Sharing::ServerSideCopy,
        }),
        CloneBackendKind::ZfsClone => Arc::new(ZfsCloneBackend {
            remote: cfg.zfs_remote.clone(),
            dataset: cfg.zfs_dataset.clone(),
        }),
        CloneBackendKind::Auto => unreachable!("probe resolves auto"),
    };
    Ok((backend, report))
}

struct ProbeDir(PathBuf);

impl Drop for ProbeDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Unique per process so parallel estimate shards do not FICLONE into the
/// same `.qs-probe/dst.bin` (EEXIST) or `remove_dir_all` each other's src.
fn exclusive_probe_dir(scratch_root: &Path) -> Result<ProbeDir> {
    let pid = std::process::id();
    for n in 0..1024u32 {
        let dir = scratch_root.join(format!(".qs-probe-{pid}-{n}"));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(ProbeDir(dir)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("create probe dir {}", dir.display())),
        }
    }
    bail!(
        "could not allocate a unique probe directory under {}",
        scratch_root.display()
    )
}

pub fn probe(
    scratch_root: &Path,
    requested: CloneBackendKind,
    allow_copy: bool,
) -> Result<ProbeReport> {
    fs::create_dir_all(scratch_root)?;
    let probe_dir = exclusive_probe_dir(scratch_root)?;
    let dir = &probe_dir.0;
    let src = dir.join("src.bin");
    let dst = dir.join("dst.bin");
    {
        let mut f = fs::File::create(&src)?;
        f.write_all(&crate::util::fill_incompressible(PROBE_BYTES))?;
        f.sync_all()?;
    }

    let try_reflink = matches!(
        requested,
        CloneBackendKind::Auto | CloneBackendKind::Reflink
    );
    if try_reflink {
        let before = crate::util::statvfs_avail(scratch_root).unwrap_or(0);
        let start = Instant::now();
        match reflink_copy::reflink(&src, &dst) {
            Ok(()) => {
                let clone_secs = start.elapsed().as_secs_f64();
                let after = crate::util::statvfs_avail(scratch_root).unwrap_or(before);
                let space_delta = before as i64 - after as i64;
                // Classify sharing before verify_independent. Writing the source
                // unshares its first CoW extent, so filefrag/FIEMAP after that
                // look like a full copy on a working XFS reflink.
                let sharing = classify_sharing(
                    space_delta,
                    PROBE_BYTES as u64,
                    filefrag_shared(&src, &dst).or_else(|| fiemap_shared(&src, &dst)),
                );
                verify_independent(&src, &dst)?;
                match sharing {
                    Sharing::SharedBlocks => {
                        let _ = fs::remove_dir_all(&dir);
                        return Ok(ProbeReport {
                            backend: CloneBackendKind::Reflink,
                            sharing,
                            message: format!(
                                "reflink clone ok in {clone_secs:.3}s; shared blocks confirmed (free-space delta {space_delta} bytes)"
                            ),
                            clone_secs,
                            space_delta,
                        });
                    }
                    other => {
                        if requested == CloneBackendKind::Reflink && !allow_copy {
                            let _ = fs::remove_dir_all(&dir);
                            bail!(reflink_not_shared_error(other, space_delta));
                        }
                        if requested == CloneBackendKind::Auto && !allow_copy {
                            // Keep the probe files; fall through to abort rather than silently copy.
                            // Auto does not select zfs-clone (no destroy path).
                            warn!(
                                "FICLONE returned success but blocks are not shared ({other:?}, delta {space_delta})"
                            );
                        } else if allow_copy {
                            let _ = fs::remove_dir_all(&dir);
                            return Ok(ProbeReport {
                                backend: CloneBackendKind::Copy,
                                sharing: other,
                                message: format!(
                                    "FICLONE succeeded but blocks are not shared (delta {space_delta} bytes). Proceeding with copy because --allow-copy was set."
                                ),
                                clone_secs,
                                space_delta,
                            });
                        }
                    }
                }
            }
            Err(err) => {
                if requested == CloneBackendKind::Reflink && !allow_copy {
                    let _ = fs::remove_dir_all(&dir);
                    bail!(ficlone_failed_error(&err));
                }
            }
        }
        let _ = fs::remove_file(&dst);
    }

    // zfs-clone is opt-in only. Auto must not pick it: `zfs snapshot -f` is
    // invalid, snapshots may cover a whole shared volume, clones are never
    // destroyed, and ensure_worker's remove_dir_all would wipe a mounted clone.
    if requested == CloneBackendKind::ZfsClone
        && zfs_available(None)
        && lookup_dataset(None, scratch_root).is_ok()
    {
        let _ = fs::remove_dir_all(&dir);
        return Ok(ProbeReport {
            backend: CloneBackendKind::ZfsClone,
            sharing: Sharing::SharedBlocks,
            message: "zfs-clone selected explicitly. Snapshots/clones are not destroyed; use a dedicated dataset, not a shared volume. An NFS client cannot issue zfs snapshot itself — set --zfs-remote if this is a re-export."
                .into(),
            clone_secs: 0.0,
            space_delta: 0,
        });
    }

    if requested == CloneBackendKind::ZfsClone {
        let _ = fs::remove_dir_all(&dir);
        bail!(
            "zfs-clone selected but `zfs` is not available, or the scratch path is not a ZFS dataset. Set --zfs-remote HOST for server-side snapshot/clone, or use --clone-backend reflink."
        );
    }

    // Copy path.
    let before = crate::util::statvfs_avail(scratch_root).unwrap_or(0);
    let start = Instant::now();
    let used_cfr = copy_file_range_path(&src, &dst).is_ok();
    if !used_cfr {
        fs::copy(&src, &dst)?;
    }
    let clone_secs = start.elapsed().as_secs_f64();
    let after = crate::util::statvfs_avail(scratch_root).unwrap_or(before);
    let space_delta = before as i64 - after as i64;
    verify_independent(&src, &dst)?;
    let sharing = if used_cfr {
        Sharing::ServerSideCopy
    } else {
        Sharing::FullCopy
    };
    let _ = fs::remove_dir_all(&dir);

    if !allow_copy {
        bail!(copy_refused_error(sharing, scratch_root));
    }
    Ok(ProbeReport {
        backend: CloneBackendKind::Copy,
        sharing,
        message: match sharing {
            Sharing::ServerSideCopy => format!(
                "WARNING: using server-side copy_file_range (real bytes, no CoW) in {clone_secs:.3}s, free-space delta {space_delta}. --allow-copy was set."
            ),
            _ => format!(
                "WARNING: using a full copy in {clone_secs:.3}s, free-space delta {space_delta}. --allow-copy was set."
            ),
        },
        clone_secs,
        space_delta,
    })
}

pub(crate) fn classify_sharing(space_delta: i64, file_size: u64, hint: Option<bool>) -> Sharing {
    if hint == Some(true) {
        return Sharing::SharedBlocks;
    }
    if hint == Some(false) {
        // Definitive: extents do not share. A noisy 0-byte statvfs delta on a
        // multi-terabyte pool must not override this.
        if space_delta > (file_size as i64) / 2 {
            return Sharing::ServerSideCopy;
        }
        return Sharing::FullCopy;
    }
    // No FIEMAP/filefrag (typical over NFS). Async free-space accounting on a
    // large pool reads ~0 whether or not blocks were shared — do not treat a
    // small delta as confirmation.
    if space_delta > (file_size as i64) / 2 {
        return Sharing::ServerSideCopy;
    }
    Sharing::Unknown
}

fn verify_independent(src: &Path, dst: &Path) -> Result<()> {
    let original_dst = fs::read(dst)?;
    {
        let mut f = fs::OpenOptions::new().write(true).open(src)?;
        f.write_all(b"MUTATED")?;
        f.sync_all()?;
    }
    let after = fs::read(dst)?;
    if after != original_dst {
        bail!("clone is not independent: mutating the source changed the destination");
    }
    Ok(())
}

fn ficlone_failed_error(err: &io::Error) -> String {
    format!(
        "clone backend probe failed: FICLONE returned {err}\n\n{FICLONE_CHAIN}\n\n\
         Alternatives:\n  --clone-backend zfs-clone   (needs QS_ZFS_REMOTE / server-side zfs)\n  \
         --clone-backend copy --allow-copy\n      (NFS v4.2 copy_file_range is a server-side byte copy, not CoW)\n\n\
         A silent fallback would turn a sub-second clone into a multi-hundred-gigabyte copy."
    )
}

fn reflink_not_shared_error(sharing: Sharing, delta: i64) -> String {
    format!(
        "FICLONE returned success but blocks are not shared ({sharing:?}, free-space delta {delta} bytes).\n\
         This is the NFS copy_file_range degradation, not CoW.\n\n{FICLONE_CHAIN}\n\n\
         Pass --allow-copy to accept a real-byte copy, or enable ZFS block cloning."
    )
}

fn copy_refused_error(sharing: Sharing, root: &Path) -> String {
    format!(
        "clone backend probe failed under {}\n\n\
         CoW clone is unavailable (observed {sharing:?}).\n\n{FICLONE_CHAIN}\n\n\
         Pass --allow-copy to proceed with a real copy. A silent fallback would turn\n\
         a sub-second clone into a multi-hundred-gigabyte copy.",
        root.display()
    )
}

fn fiemap_shared(a: &Path, b: &Path) -> Option<bool> {
    let ea = fiemap_first_physical(a)?;
    let eb = fiemap_first_physical(b)?;
    if ea == 0 || eb == 0 {
        return None;
    }
    Some(ea == eb)
}

/// XFS `filefrag -v` prints `shared` on extents that share physical blocks.
fn filefrag_shared(a: &Path, b: &Path) -> Option<bool> {
    fn extents(path: &Path) -> Option<(bool, String)> {
        let cmd = if std::path::Path::new("/usr/sbin/filefrag").is_file() {
            "/usr/sbin/filefrag"
        } else {
            "filefrag"
        };
        let out = std::process::Command::new(cmd)
            .arg("-v")
            .arg(path)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut phys = String::new();
        let mut shared = false;
        for line in text.lines() {
            if line.contains("shared") {
                shared = true;
            }
            // "0:        0..    8191:   79906503..  79914694:"
            if let Some(rest) = line.split_once(':') {
                if rest.0.trim().chars().all(|c| c.is_ascii_digit()) {
                    phys.push_str(line.trim());
                    phys.push('\n');
                }
            }
        }
        Some((shared, phys))
    }
    let (sa, pa) = extents(a)?;
    let (sb, pb) = extents(b)?;
    if pa.is_empty() || pb.is_empty() {
        return None;
    }
    Some(sa && sb && pa == pb)
}

fn fiemap_first_physical(path: &Path) -> Option<u64> {
    use std::os::unix::io::AsRawFd;
    const FS_IOC_FIEMAP: libc::c_ulong = 0xC020660B;
    #[repr(C)]
    struct FiemapExtent {
        fe_logical: u64,
        fe_physical: u64,
        fe_length: u64,
        fe_reserved64: [u64; 2],
        fe_flags: u32,
        fe_reserved: [u32; 3],
    }
    #[repr(C)]
    struct Fiemap {
        fm_start: u64,
        fm_length: u64,
        fm_flags: u32,
        fm_mapped_extents: u32,
        fm_extent_count: u32,
        fm_reserved: u32,
        fm_extents: FiemapExtent,
    }
    let file = fs::File::open(path).ok()?;
    let mut fm = Fiemap {
        fm_start: 0,
        fm_length: u64::MAX,
        fm_flags: 0,
        fm_mapped_extents: 0,
        fm_extent_count: 1,
        fm_reserved: 0,
        fm_extents: FiemapExtent {
            fe_logical: 0,
            fe_physical: 0,
            fe_length: 0,
            fe_reserved64: [0; 2],
            fe_flags: 0,
            fe_reserved: [0; 3],
        },
    };
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP as _, &mut fm) };
    if rc != 0 || fm.fm_mapped_extents == 0 {
        return None;
    }
    Some(fm.fm_extents.fe_physical)
}

fn copy_file_range_path(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let src_f = fs::File::open(src)?;
    let dst_f = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(dst)?;
    let meta = src_f.metadata()?;
    let mut off_in: i64 = 0;
    let mut off_out: i64 = 0;
    let mut left = meta.len();
    while left > 0 {
        let n = unsafe {
            libc::copy_file_range(
                src_f.as_raw_fd(),
                &mut off_in,
                dst_f.as_raw_fd(),
                &mut off_out,
                left.min(usize::MAX as u64) as usize,
                0,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n == 0 {
            break;
        }
        left -= n as u64;
    }
    if left != 0 {
        return Err(io::Error::other("copy_file_range short write"));
    }
    // A new file gets the umask's mode, not the source's: executables such
    // as build-script-build in a cloned target/ would lose their x bit.
    dst_f.set_permissions(meta.permissions())?;
    dst_f.set_times(times_of(&meta)?)?;
    Ok(())
}

fn times_of(meta: &fs::Metadata) -> io::Result<fs::FileTimes> {
    Ok(fs::FileTimes::new()
        .set_accessed(meta.accessed()?)
        .set_modified(meta.modified()?))
}

/// Give `dst` the access and modification times of `src`. Cargo decides
/// whether a workspace unit is fresh by comparing source and output
/// modification times. A copy that stamps every file with the copy time, in
/// whatever order the copy threads ran, made cargo rebuild 36 of itertools'
/// 61 units (build scripts of registry crates included) in every fresh
/// worker, with no source change; with the times kept, all 61 are fresh.
pub fn keep_times(src: &Path, dst: &Path) -> io::Result<()> {
    let times = times_of(&fs::metadata(src)?)?;
    fs::File::open(dst)?.set_times(times)
}

fn clone_tree_with(backend: &dyn CloneBackend, src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        fs::remove_dir_all(dst).with_context(|| format!("remove existing {}", dst.display()))?;
    }
    fs::create_dir_all(dst)?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut links = Vec::new();
    let walker = WalkBuilder::new(src)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .standard_filters(false)
        .build();
    for entry in walker {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(src).unwrap_or(path);
        if rel.as_os_str().is_empty() {
            continue;
        }
        let dest = dst.join(rel);
        let ft = entry.file_type();
        if ft.map(|t| t.is_dir()).unwrap_or(false) {
            dirs.push(dest);
        } else if ft.map(|t| t.is_symlink()).unwrap_or(false) {
            links.push((path.to_path_buf(), dest));
        } else if ft.map(|t| t.is_file()).unwrap_or(false) {
            files.push((path.to_path_buf(), dest));
        }
    }
    dirs.sort_by_key(|p| p.as_os_str().len());
    for d in dirs {
        fs::create_dir_all(d)?;
    }
    for (s, d) in links {
        if let Some(parent) = d.parent() {
            fs::create_dir_all(parent)?;
        }
        let target = fs::read_link(&s)?;
        let _ = fs::remove_file(&d);
        symlink(&target, &d)
            .with_context(|| format!("symlink {} -> {}", target.display(), d.display()))?;
    }
    let nthreads = crate::util::nproc().min(32).max(1);
    if files.len() < 64 {
        for (s, d) in files {
            backend.clone_file(&s, &d)?;
        }
        return Ok(());
    }
    let (tx, rx) = crossbeam_channel::unbounded::<(PathBuf, PathBuf)>();
    for pair in files {
        tx.send(pair).unwrap();
    }
    drop(tx);
    std::thread::scope(|scope| {
        let mut joins = Vec::new();
        for _ in 0..nthreads {
            let rx = rx.clone();
            joins.push(scope.spawn(move || -> Result<()> {
                while let Ok((s, d)) = rx.recv() {
                    backend.clone_file(&s, &d)?;
                }
                Ok(())
            }));
        }
        for j in joins {
            j.join().expect("clone thread panicked")?;
        }
        Ok(())
    })
}

pub fn materialize_baseline(repo: &Path, baseline: &Path) -> Result<()> {
    if baseline.exists() {
        info!("replacing existing baseline {}", baseline.display());
        fs::remove_dir_all(baseline)?;
    }
    fs::create_dir_all(baseline)?;
    let walker = WalkBuilder::new(repo)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .filter_entry(|e| {
            let name = e.file_name();
            name != "target" && name != ".git"
        })
        .build();
    for entry in walker {
        let entry = entry?;
        let path = entry.path();
        let rel = match path.strip_prefix(repo) {
            Ok(r) if !r.as_os_str().is_empty() => r,
            _ => continue,
        };
        let dest = baseline.join(rel);
        let ft = entry.file_type();
        if ft.map(|t| t.is_dir()).unwrap_or(false) {
            fs::create_dir_all(&dest)?;
        } else if ft.map(|t| t.is_symlink()).unwrap_or(false) {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            let target = fs::read_link(path)?;
            let _ = fs::remove_file(&dest);
            symlink(target, &dest)?;
        } else if ft.map(|t| t.is_file()).unwrap_or(false) {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(path, &dest)?;
        }
    }
    copy_git(repo, baseline)?;
    write_quicksilver_cargo_config(baseline)?;
    Ok(())
}

/// What [`sync_baseline`] changed.
#[derive(Debug, Default, Clone, Copy)]
pub struct SyncReport {
    pub copied: usize,
    pub removed: usize,
    pub git_refreshed: bool,
}

/// Bring an existing baseline copy up to date with `repo` in place. Only
/// files whose content differs are rewritten, so cargo's incremental state
/// stays valid for the rest. `target/` and Bifrost's `.bifrost/cache/` are
/// never touched; replacing the baseline would throw both away on every
/// commit.
pub fn sync_baseline(repo: &Path, baseline: &Path) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let walker = WalkBuilder::new(repo)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .filter_entry(|e| {
            let name = e.file_name();
            name != "target" && name != ".git"
        })
        .build();
    for entry in walker {
        let entry = entry?;
        let path = entry.path();
        let rel = match path.strip_prefix(repo) {
            Ok(r) if !r.as_os_str().is_empty() => r.to_path_buf(),
            _ => continue,
        };
        let dest = baseline.join(&rel);
        let ft = entry.file_type();
        seen.insert(rel);
        if ft.map(|t| t.is_dir()).unwrap_or(false) {
            fs::create_dir_all(&dest)?;
        } else if ft.map(|t| t.is_symlink()).unwrap_or(false) {
            let target = fs::read_link(path)?;
            if fs::read_link(&dest).ok().as_ref() != Some(&target) {
                if let Some(p) = dest.parent() {
                    fs::create_dir_all(p)?;
                }
                let _ = fs::remove_file(&dest);
                symlink(target, &dest)?;
                report.copied += 1;
            }
        } else if ft.map(|t| t.is_file()).unwrap_or(false) && !same_file_content(path, &dest)? {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            if fs::symlink_metadata(&dest).is_ok_and(|m| !m.is_file()) {
                let _ = fs::remove_file(&dest);
            }
            fs::copy(path, &dest)?;
            report.copied += 1;
        }
    }
    let quicksilver_config = PathBuf::from(".cargo/config.toml");
    let stale: Vec<PathBuf> = WalkBuilder::new(baseline)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .filter_entry(|e| {
            let name = e.file_name();
            name != "target" && name != ".git" && !e.path().ends_with(".bifrost/cache")
        })
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|t| !t.is_dir()))
        .filter_map(|e| {
            let rel = e.path().strip_prefix(baseline).ok()?.to_path_buf();
            (!seen.contains(&rel) && rel != quicksilver_config).then_some(rel)
        })
        .collect();
    for rel in stale {
        fs::remove_file(baseline.join(&rel))?;
        report.removed += 1;
    }
    let head = |dir: &Path| git_output(dir, &["rev-parse", "HEAD"]).ok();
    if !baseline.join(".git").exists() || head(repo) != head(baseline) {
        let _ = fs::remove_dir_all(baseline.join(".git"));
        copy_git(repo, baseline)?;
        report.git_refreshed = true;
    }
    write_quicksilver_cargo_config(baseline)?;
    Ok(report)
}

fn same_file_content(a: &Path, b: &Path) -> Result<bool> {
    let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::symlink_metadata(b)) else {
        return Ok(false);
    };
    if !mb.is_file() || ma.len() != mb.len() {
        return Ok(false);
    }
    Ok(fs::read(a)? == fs::read(b)?)
}

/// Profile lives in the materialized baseline, not in the user's repo.
pub fn write_quicksilver_cargo_config(baseline: &Path) -> Result<()> {
    let dir = baseline.join(".cargo");
    fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");
    let mut body = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    if !body.contains("[profile.quicksilver]") {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(
            "\n[profile.quicksilver]\ninherits = \"dev\"\ndebug = 0\nincremental = true\n",
        );
        fs::write(&path, body)?;
    }
    Ok(())
}

fn copy_git(src: &Path, dst: &Path) -> Result<()> {
    let git_entry = src.join(".git");
    if git_entry.is_dir() {
        copy_dir_plain(&git_entry, &dst.join(".git"))?;
        return Ok(());
    }
    if !git_entry.exists() {
        return Ok(());
    }
    // Worktree: copy the common dir so build scripts can run git log / git diff.
    let common = git_output(src, &["rev-parse", "--git-common-dir"])?;
    let common_path = resolve_git_path(src, &common);
    if common_path.is_dir() {
        copy_dir_plain(&common_path, &dst.join(".git"))?;
        if let Ok(head) = git_output(src, &["rev-parse", "HEAD"]) {
            fs::write(dst.join(".git/HEAD"), format!("{head}\n"))?;
        }
    } else {
        fs::copy(&git_entry, dst.join(".git"))?;
    }
    Ok(())
}

fn resolve_git_path(src: &Path, printed: &str) -> PathBuf {
    let p = PathBuf::from(printed);
    if p.is_absolute() {
        p
    } else {
        src.join(p)
    }
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn copy_dir_plain(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in WalkBuilder::new(src)
        .hidden(false)
        .standard_filters(false)
        .build()
    {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src).unwrap_or(entry.path());
        if rel.as_os_str().is_empty() {
            continue;
        }
        let dest = dst.join(rel);
        let ft = entry.file_type();
        if ft.map(|t| t.is_dir()).unwrap_or(false) {
            fs::create_dir_all(dest)?;
        } else if ft.map(|t| t.is_file()).unwrap_or(false) {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn zfs_available(remote: Option<&str>) -> bool {
    let mut cmd = zfs_cmd(remote);
    cmd.args(["list", "-H"]);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

fn zfs_cmd(remote: Option<&str>) -> Command {
    if let Some(host) = remote {
        let mut cmd = Command::new("ssh");
        cmd.arg(host).arg("zfs");
        cmd
    } else {
        Command::new("zfs")
    }
}

fn zfs_clone_tree(backend: &ZfsCloneBackend, src: &Path, dst: &Path) -> Result<()> {
    let remote = backend.remote.as_deref();
    let src = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
    let dataset = match &backend.dataset {
        Some(d) => d.clone(),
        None => lookup_dataset(remote, &src)?,
    };
    let snap = format!(
        "qs-{}",
        crate::util::sha256_bytes(dst.to_string_lossy().as_bytes())[..12].to_string()
    );
    let snap_name = format!("{dataset}@{snap}");
    let clone_name = format!("{dataset}-{}", crate::util::sanitize_filename(&snap));
    let mut snap_cmd = zfs_cmd(remote);
    // `zfs snapshot` has no `-f`. Force-destroy is `zfs destroy -f`.
    snap_cmd.args(["snapshot", &snap_name]);
    let status = snap_cmd.status()?;
    if !status.success() {
        // snapshot may already exist from a previous leaked clone
        let mut s2 = zfs_cmd(remote);
        s2.args(["snapshot", &snap_name]);
        let _ = s2.status()?;
    }
    if dst.exists() {
        // Do not remove_dir_all a mounted ZFS clone — that deletes dataset
        // contents rather than the mount. Fail instead; this backend has no
        // destroy path yet (which is why Auto will not select it).
        bail!(
            "zfs-clone destination {} already exists; refusing to delete a possible mounted dataset. Destroy the clone dataset explicitly.",
            dst.display()
        );
    }
    let mut clone = zfs_cmd(remote);
    clone.args([
        "clone",
        "-o",
        &format!("mountpoint={}", dst.display()),
        &snap_name,
        &clone_name,
    ]);
    let out = clone.output().context("zfs clone")?;
    if !out.status.success() {
        bail!(
            "zfs clone failed: {}\n{}",
            String::from_utf8_lossy(&out.stderr),
            "An NFS client cannot issue zfs snapshot/clone; set --zfs-remote to the server (morannon)."
        );
    }
    Ok(())
}

fn lookup_dataset(remote: Option<&str>, mount: &Path) -> Result<String> {
    let mut cmd = zfs_cmd(remote);
    cmd.args(["list", "-H", "-o", "name,mountpoint"]);
    let out = cmd.output().context("zfs list")?;
    if !out.status.success() {
        bail!("zfs list failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut best: Option<(usize, String)> = None;
    for line in text.lines() {
        let mut cols = line.split('\t');
        let Some(name) = cols.next() else { continue };
        let Some(mp) = cols.next() else { continue };
        if mp == "-" {
            continue;
        }
        let mp = Path::new(mp);
        if mount.starts_with(mp) {
            let len = mp.as_os_str().len();
            if best.as_ref().map(|(l, _)| len >= *l).unwrap_or(true) {
                best = Some((len, name.to_string()));
            }
        }
    }
    best.map(|(_, n)| n)
        .context("no ZFS dataset mounts at the scratch path")
}

pub fn worker_dir(paths: &Paths, slot: usize) -> PathBuf {
    let prefix = std::env::var("QS_WORKER_PREFIX").unwrap_or_default();
    paths.workers.join(format!("{prefix}w{slot:02}"))
}

pub fn ensure_worker(backend: &dyn CloneBackend, paths: &Paths, slot: usize) -> Result<PathBuf> {
    let dest = worker_dir(paths, slot);
    if dest.exists() {
        if backend.name() == "zfs-clone" {
            bail!(
                "zfs-clone worker {} already exists; refusing to delete a possible mounted dataset",
                dest.display()
            );
        }
        fs::remove_dir_all(&dest)?;
    }
    info!("cloning baseline -> {}", dest.display());
    let start = Instant::now();
    backend.clone_tree(&paths.baseline, &dest)?;
    info!(
        "cloned baseline -> {} in {:.1}s via {}",
        dest.display(),
        start.elapsed().as_secs_f64(),
        backend.name()
    );
    private_target_dir(&dest)?;
    Ok(dest)
}

/// Give a copied tree its own `target` directory when the copy brought a
/// symlink. A build cache such as mbx makes `target` a symlink to a managed
/// directory; copied as a symlink, it makes the baseline and every worker
/// build into one directory, so concurrent schema builds of different crates
/// overwrite each other's artifacts and a crate's tests run against a library
/// without its sites. The new directory starts empty: its first build is a
/// full one, which a build cache serves from its store.
pub fn private_target_dir(tree: &Path) -> Result<()> {
    let target = tree.join("target");
    let is_link = fs::symlink_metadata(&target)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_link {
        let shared = fs::read_link(&target).unwrap_or_default();
        fs::remove_file(&target)
            .with_context(|| format!("remove copied target symlink {}", target.display()))?;
        fs::create_dir_all(&target)?;
        info!(
            "{} was a symlink to {}; replaced it with an empty private directory",
            target.display(),
            shared.display()
        );
    }
    Ok(())
}

pub fn check_free_space(path: &Path, min: Option<u64>) -> Result<()> {
    let avail = crate::util::statvfs_avail(path)?;
    if let Some(min) = min {
        if avail < min {
            bail!("scratch filesystem has {avail} bytes free; --max-scratch wants at least {min}");
        }
    }
    if avail < 5 * (1 << 30) {
        warn!("scratch filesystem has only {} bytes free", avail);
    }
    Ok(())
}

pub fn format_probe(report: &ProbeReport) -> String {
    format!(
        "clone backend: {}\n  {}\n  sharing: {:?}\n  probe clone: {:.3}s",
        report.backend.as_str(),
        report.message,
        report.sharing,
        report.clone_secs
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cargo's freshness check compares modification times. A worker copy
    /// that restamps every file made cargo rebuild most of the workspace.
    #[test]
    fn cloned_files_keep_their_modification_times() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, SystemTime};
        let dir = tempfile::tempdir().unwrap();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for (name, mode) in [("plain", 0o644), ("read_only", 0o444)] {
            let src = dir.path().join(name);
            fs::write(&src, b"contents").unwrap();
            fs::File::options()
                .write(true)
                .open(&src)
                .unwrap()
                .set_modified(old)
                .unwrap();
            fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();
            for (label, backend) in [
                ("copy_file_range", CopyBackend { server_side: true }),
                ("copy", CopyBackend { server_side: false }),
            ] {
                let dst = dir.path().join(format!("{name}-{label}"));
                backend.clone_file(&src, &dst).unwrap();
                assert_eq!(
                    fs::metadata(&dst).unwrap().modified().unwrap(),
                    old,
                    "{name} via {label}"
                );
            }
        }
    }

    /// mbx makes `target` a symlink to a managed directory. A worker cloned
    /// with that symlink built into the same directory as the baseline and
    /// every other worker; on clap, concurrent schema builds overwrote each
    /// other and clap_builder's tests recorded none of its 911 sites.
    #[test]
    fn a_worker_gets_a_private_target_when_the_baseline_target_is_a_symlink() {
        let scratch = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        let paths = Paths::for_session(scratch.path(), Path::new("/repo"));
        paths.ensure().unwrap();
        fs::create_dir_all(paths.baseline.join("src")).unwrap();
        fs::write(paths.baseline.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        fs::write(shared.path().join("artifact"), "shared build output").unwrap();
        std::os::unix::fs::symlink(shared.path(), paths.baseline.join("target")).unwrap();

        let backend = CopyBackend { server_side: false };
        let worker = ensure_worker(&backend, &paths, 0).unwrap();
        let target = fs::symlink_metadata(worker.join("target")).unwrap();
        assert!(
            target.is_dir() && !target.file_type().is_symlink(),
            "the worker's target must be its own directory"
        );
        assert!(!worker.join("target/artifact").exists());
        assert!(worker.join("src/lib.rs").is_file());
        assert!(
            shared.path().join("artifact").is_file(),
            "the shared directory itself is left alone"
        );

        // A real target directory is a private copy already and is kept.
        fs::write(worker.join("target/keep"), "own build output").unwrap();
        private_target_dir(&worker).unwrap();
        assert!(worker.join("target/keep").is_file());
    }

    #[test]
    fn sync_baseline_updates_in_place_and_keeps_build_state() {
        let repo = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let w = |root: &Path, rel: &str, body: &str| {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        };
        w(repo.path(), "Cargo.toml", "[package]\nname = \"x\"\n");
        w(repo.path(), "src/lib.rs", "pub fn a() {}\n");
        w(repo.path(), "src/gone.rs", "// removed later\n");
        w(repo.path(), ".gitignore", "/target\n");
        materialize_baseline(repo.path(), base.path()).unwrap();
        w(base.path(), "target/debug/keep", "build state");
        w(
            base.path(),
            ".bifrost/cache/bifrost_cache.db",
            "analysis state",
        );
        let untouched = fs::metadata(base.path().join("Cargo.toml"))
            .unwrap()
            .modified()
            .unwrap();

        w(repo.path(), "src/lib.rs", "pub fn a() { let _ = 1; }\n");
        w(repo.path(), "src/new.rs", "pub fn b() {}\n");
        fs::remove_file(repo.path().join("src/gone.rs")).unwrap();
        let report = sync_baseline(repo.path(), base.path()).unwrap();

        assert_eq!((report.copied, report.removed), (2, 1));
        assert_eq!(
            fs::read_to_string(base.path().join("src/lib.rs")).unwrap(),
            "pub fn a() { let _ = 1; }\n"
        );
        assert!(base.path().join("src/new.rs").is_file());
        assert!(!base.path().join("src/gone.rs").exists());
        assert!(base.path().join("target/debug/keep").is_file());
        assert!(base
            .path()
            .join(".bifrost/cache/bifrost_cache.db")
            .is_file());
        assert!(base.path().join(".cargo/config.toml").is_file());
        assert_eq!(
            fs::metadata(base.path().join("Cargo.toml"))
                .unwrap()
                .modified()
                .unwrap(),
            untouched,
            "unchanged files are not rewritten"
        );
        let again = sync_baseline(repo.path(), base.path()).unwrap();
        assert_eq!((again.copied, again.removed), (0, 0));
    }

    #[test]
    fn copy_file_range_keeps_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("build-script-build");
        fs::write(&src, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
        let dst = dir.path().join("copy");
        copy_file_range_path(&src, &dst).unwrap();
        assert_eq!(
            fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn hint_true_is_shared_even_if_delta_is_large() {
        assert_eq!(
            classify_sharing(32 * 1024 * 1024, 32 * 1024 * 1024, Some(true)),
            Sharing::SharedBlocks
        );
    }

    #[test]
    fn hint_false_is_not_overridden_by_small_delta() {
        assert_eq!(
            classify_sharing(0, 32 * 1024 * 1024, Some(false)),
            Sharing::FullCopy
        );
    }

    #[test]
    fn no_hint_small_delta_is_unknown_not_shared() {
        // NFS/ZFS async accounting: 32 MB is noise on a 6.8 T pool.
        assert_eq!(
            classify_sharing(0, 32 * 1024 * 1024, None),
            Sharing::Unknown
        );
    }

    #[test]
    fn no_hint_large_delta_is_a_copy() {
        assert_eq!(
            classify_sharing(32 * 1024 * 1024, 32 * 1024 * 1024, None),
            Sharing::ServerSideCopy
        );
    }

    #[test]
    fn exclusive_probe_dir_does_not_collide() {
        let tmp = tempfile::tempdir().unwrap();
        let a = exclusive_probe_dir(tmp.path()).unwrap();
        let b = exclusive_probe_dir(tmp.path()).unwrap();
        assert_ne!(a.0, b.0);
        assert!(a.0.is_dir());
        assert!(b.0.is_dir());
    }

    #[test]
    fn concurrent_probes_do_not_share_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::thread::scope(|scope| {
            let joins: Vec<_> = (0..4)
                .map(|_| {
                    let root = root.clone();
                    scope.spawn(move || probe(&root, CloneBackendKind::Auto, true))
                })
                .collect();
            for j in joins {
                j.join()
                    .unwrap()
                    .expect("parallel probe must not race on the probe directory");
            }
        });
    }
}
