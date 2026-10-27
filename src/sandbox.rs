//! A fresh home and temporary directory for each test process QS starts,
//! so that tests, and the mutants they run, cannot write the user's own
//! state: caches under `~/.cache`, config files, files left in `/tmp`.
//!
//! Each process gets `HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`,
//! `XDG_DATA_HOME`, `XDG_STATE_HOME` and `TMPDIR` in its own directory,
//! deleted when the process exits. The directories of one QS run are in a
//! private `qs-sandbox-*` directory under the system temporary directory,
//! or under `[sandbox] dir`. That place must be outside any git repository:
//! under `.git`, git sees a test's temporary directory as part of the
//! repository. Its path should also be short, because a Unix socket's path
//! must fit in about 100 bytes. A small record under QS's state names the
//! run's directory, so a later run can delete it if QS was killed.
//!
//! Tools that tests may run keep working:
//! `CARGO_HOME`, `RUSTUP_HOME` and `MBX_CACHE_DIR` point at the real ones,
//! and the git and mbx config files are copied in.
//!
//! The baseline `cargo nextest run` is not sandboxed: it runs unmutated code
//! through mbx, which uses `HOME` for its C compiler path mappings and for
//! `~` in its config.

use crate::config::Config;
use crate::types::Paths;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// The start of the name of each QS run's sandbox directory. Stale-run
/// cleanup deletes only directories named like this.
const ROOT_PREFIX: &str = "qs-sandbox-";

/// The placeholder in `[test-env]` values for the process's sandbox.
pub const PLACEHOLDER: &str = "{sandbox}";

/// How every test process QS starts is set up.
#[derive(Debug)]
pub struct Settings {
    /// Where this QS process's sandbox directory is made; `None` when the
    /// sandbox is off.
    parent: Option<PathBuf>,
    /// This QS process's `qs-sandbox-*` directory, made on first use.
    root: OnceLock<std::result::Result<PathBuf, String>>,
    /// The file under QS's state that records `root`, for cleanup after a
    /// crash; `None` keeps no record.
    record: Option<PathBuf>,
    /// Variables every sandboxed process gets, such as the real `CARGO_HOME`.
    fixed: Vec<(String, String)>,
    /// Files copied into each sandbox: (real file, path under the sandbox).
    copies: Vec<(PathBuf, PathBuf)>,
    /// `[test-env]`, with the placeholder still in the values.
    test_env: BTreeMap<String, String>,
    next: AtomicU64,
    /// Set by [`Settings::finish`]: from then on, the last sandbox to go
    /// also removes the run's directory.
    finished: std::sync::atomic::AtomicBool,
    processes: AtomicU64,
    largest: AtomicU64,
    kept: Mutex<Vec<(PathBuf, String)>>,
}

/// The user's own directories, from the environment QS runs in.
#[derive(Debug, Clone, Default)]
pub struct RealDirs {
    pub home: Option<PathBuf>,
    /// Git's global config file below `XDG_CONFIG_HOME` (or `~/.config`).
    pub git_config: Option<PathBuf>,
    /// mbx's config file, in the platform config directory.
    pub mbx_config: Option<PathBuf>,
    pub cargo_home: Option<PathBuf>,
    pub rustup_home: Option<PathBuf>,
    pub mbx_cache: Option<PathBuf>,
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// Where mbx (through the `dirs` crate) looks for its config file, for a
/// home and `XDG_CONFIG_HOME`.
fn mbx_config_path(home: &Path, xdg_config: &Path) -> PathBuf {
    let dir = if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        xdg_config.to_path_buf()
    };
    dir.join("mbx").join("config.toml")
}

impl RealDirs {
    pub fn from_env() -> RealDirs {
        let home = env_dir("HOME");
        let xdg_config = home
            .as_deref()
            .map(|h| env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| h.join(".config")));
        let mbx_config = home
            .as_deref()
            .zip(xdg_config.as_deref())
            .map(|(h, x)| mbx_config_path(h, x));
        let cache = home.as_deref().map(|h| {
            if cfg!(target_os = "macos") {
                h.join("Library").join("Caches")
            } else {
                env_dir("XDG_CACHE_HOME").unwrap_or_else(|| h.join(".cache"))
            }
        });
        // The same order mbx resolves its cache in: the variable, the
        // config file's `cache_dir`, then the platform cache directory.
        let mbx_cache = env_dir("MBX_CACHE_DIR")
            .or_else(|| {
                let text = fs::read_to_string(mbx_config.as_deref()?).ok()?;
                let doc: toml::Value = toml::from_str(&text).ok()?;
                doc.get("cache_dir")?.as_str().map(PathBuf::from)
            })
            .or_else(|| cache.as_ref().map(|c| c.join("mbx")));
        RealDirs {
            cargo_home: env_dir("CARGO_HOME").or_else(|| home.as_ref().map(|h| h.join(".cargo"))),
            rustup_home: env_dir("RUSTUP_HOME")
                .or_else(|| home.as_ref().map(|h| h.join(".rustup"))),
            home,
            git_config: xdg_config.map(|x| x.join("git").join("config")),
            mbx_config,
            mbx_cache,
        }
    }
}

/// The variables a sandbox at `dir` sets, before the fixed ones.
fn sandbox_vars(dir: &Path) -> Vec<(String, PathBuf)> {
    let home = dir.join("home");
    vec![
        ("XDG_CONFIG_HOME".into(), home.join(".config")),
        ("XDG_CACHE_HOME".into(), home.join(".cache")),
        ("XDG_DATA_HOME".into(), home.join(".local").join("share")),
        ("XDG_STATE_HOME".into(), home.join(".local").join("state")),
        ("TMPDIR".into(), dir.join("tmp")),
        ("HOME".into(), home),
    ]
}

impl Settings {
    /// Settings with sandboxes under `parent` (none when off) and
    /// `[test-env]`.
    pub fn new(
        parent: Option<PathBuf>,
        test_env: BTreeMap<String, String>,
        real: &RealDirs,
    ) -> Result<Settings> {
        for (key, value) in &test_env {
            if key.starts_with("QS_") {
                bail!("[test-env] cannot set {key}: QS_ variables are quicksilver's own");
            }
            if parent.is_none() && value.contains(PLACEHOLDER) {
                bail!("[test-env] {key} uses {PLACEHOLDER}, but the sandbox is off");
            }
        }
        let mut fixed = Vec::new();
        for (key, dir) in [
            ("CARGO_HOME", &real.cargo_home),
            ("RUSTUP_HOME", &real.rustup_home),
            ("MBX_CACHE_DIR", &real.mbx_cache),
        ] {
            if let Some(dir) = dir {
                fixed.push((key.to_string(), dir.to_string_lossy().into_owned()));
            }
        }
        // Paths under the sandbox match `sandbox_vars`.
        let home = Path::new("home");
        let xdg_config = home.join(".config");
        let mut copies = Vec::new();
        if let Some(real_home) = &real.home {
            copies.push((real_home.join(".gitconfig"), home.join(".gitconfig")));
        }
        if let Some(git) = &real.git_config {
            copies.push((git.clone(), xdg_config.join("git").join("config")));
        }
        if let Some(mbx) = &real.mbx_config {
            copies.push((mbx.clone(), mbx_config_path(home, &xdg_config)));
        }
        copies.retain(|(from, _)| from.is_file());
        Ok(Settings {
            parent,
            root: OnceLock::new(),
            record: None,
            fixed,
            copies,
            test_env,
            next: AtomicU64::new(0),
            finished: std::sync::atomic::AtomicBool::new(false),
            processes: AtomicU64::new(0),
            largest: AtomicU64::new(0),
            kept: Mutex::new(Vec::new()),
        })
    }

    /// Set up `cmd` to run in a new sandbox, which the returned guard
    /// deletes when dropped. Without a sandbox, only `[test-env]` is set.
    pub fn prepare(self: &Arc<Self>, cmd: &mut Command) -> Result<Option<Sandbox>> {
        let Some(parent) = &self.parent else {
            for (key, value) in &self.test_env {
                cmd.env(key, value);
            }
            return Ok(None);
        };
        let root = self
            .root
            .get_or_init(|| self.make_root(parent))
            .as_ref()
            .map_err(|why| anyhow::anyhow!("{why}"))?;
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let dir = root.join(n.to_string());
        for (_, path) in sandbox_vars(&dir) {
            fs::create_dir_all(&path)
                .with_context(|| format!("create sandbox directory {}", path.display()))?;
        }
        for (from, to) in &self.copies {
            let to = dir.join(to);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(from, &to)
                .with_context(|| format!("copy {} into the sandbox", from.display()))?;
        }
        for (key, path) in sandbox_vars(&dir) {
            cmd.env(key, path);
        }
        for (key, value) in &self.fixed {
            cmd.env(key, value);
        }
        let dir_text = dir.to_string_lossy();
        for (key, value) in &self.test_env {
            cmd.env(key, value.replace(PLACEHOLDER, &dir_text));
        }
        Ok(Some(Sandbox {
            settings: Arc::clone(self),
            dir,
        }))
    }

    /// Make this QS process's sandbox directory under `parent` and record it.
    fn make_root(&self, parent: &Path) -> std::result::Result<PathBuf, String> {
        fs::create_dir_all(parent)
            .map_err(|err| format!("create the sandbox directory {}: {err}", parent.display()))?;
        let root = private_dir(parent)
            .ok_or_else(|| format!("cannot make a sandbox directory under {}", parent.display()))?;
        if let Some(record) = &self.record {
            let saved = record
                .parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| fs::write(record, root.to_string_lossy().as_bytes()));
            if let Err(err) = saved {
                tracing::warn!(
                    "cannot record the sandbox directory in {}: {err}; if quicksilver is killed, delete {} by hand",
                    record.display(),
                    root.display()
                );
            }
        }
        Ok(root)
    }

    /// Whether `[test-env]` sets this variable.
    pub fn sets(&self, name: &str) -> bool {
        self.test_env.contains_key(name)
    }

    /// This QS process's sandbox directory, once a test process used it.
    pub fn root(&self) -> Option<&Path> {
        self.root.get()?.as_ref().ok().map(PathBuf::as_path)
    }

    /// One line on the sandboxes used so far, or `None` when there were none.
    pub fn summary(&self) -> Option<String> {
        let processes = self.processes.load(Ordering::Relaxed);
        if processes == 0 {
            return None;
        }
        let largest = human_bytes(self.largest.load(Ordering::Relaxed));
        let kept = self.kept.lock().unwrap();
        let fate = match kept.first() {
            None => "all deleted".to_string(),
            Some((path, why)) => format!(
                "{} could not be deleted, for example {} ({why})",
                kept.len(),
                path.display()
            ),
        };
        Some(format!(
            "sandbox: {processes} test processes ran with their own HOME and TMPDIR; the largest left {largest}; {fate}"
        ))
    }
}

/// One test process's sandbox, deleted on drop.
pub struct Sandbox {
    settings: Arc<Settings>,
    dir: PathBuf,
}

impl Sandbox {
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let size = tree_size(&self.dir);
        let s = &self.settings;
        s.processes.fetch_add(1, Ordering::Relaxed);
        s.largest.fetch_max(size, Ordering::Relaxed);
        if let Err(err) = remove_tree(&self.dir) {
            s.kept
                .lock()
                .unwrap()
                .push((self.dir.clone(), err.to_string()));
        }
        // A test process that started after the run finished (another test
        // in the same process, in QS's own tests) made the directory again.
        if s.finished.load(Ordering::SeqCst) {
            if let Some(root) = self.dir.parent() {
                let _ = fs::remove_dir(root);
            }
        }
    }
}

/// A new directory under `parent` that only this user can enter, with a
/// random name, or `None` if one cannot be made.
fn private_dir(parent: &Path) -> Option<PathBuf> {
    use rand::Rng;
    for _ in 0..8 {
        let name: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(8)
            .map(char::from)
            .collect();
        let path = parent.join(format!("{ROOT_PREFIX}{name}"));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => return Some(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

/// Bytes in the files under `dir`, not following symbolic links.
fn tree_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// Delete `dir`, first making its directories writable again if a test
/// took the write permission away.
fn remove_tree(dir: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut stack = vec![dir.to_path_buf()];
                while let Some(d) = stack.pop() {
                    let _ = fs::set_permissions(&d, fs::Permissions::from_mode(0o700));
                    if let Ok(entries) = fs::read_dir(&d) {
                        for entry in entries.flatten() {
                            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                                stack.push(entry.path());
                            }
                        }
                    }
                }
            }
            fs::remove_dir_all(dir)
        }
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

static SETTINGS: RwLock<Option<Arc<Settings>>> = RwLock::new(None);

/// The settings test processes run with, once [`configure`] has run.
pub fn current() -> Option<Arc<Settings>> {
    SETTINGS.read().unwrap().clone()
}

/// Turn the sandbox on (unless `cfg` turns it off) for every test process
/// this QS process starts, with `[test-env]`. The sandboxes go under
/// `[sandbox] dir` or the system temporary directory, which must be outside
/// any git repository. Sandboxes of QS processes that are gone are deleted.
pub fn configure(cfg: &Config, paths: &Paths) -> Result<()> {
    let records = paths.session.join("sandbox");
    let parent = if cfg.sandbox {
        let parent = cfg.sandbox_dir.clone().unwrap_or_else(std::env::temp_dir);
        if let Some(git_dir) = enclosing_repository(&parent) {
            bail!(
                "test sandboxes would go under {}, which git sees as inside the repository at {}, so tests that expect their temporary directory outside a repository would fail; set `dir` under [sandbox] in quicksilver.toml to a directory outside any repository, or pass --no-sandbox",
                parent.display(),
                git_dir
            );
        }
        remove_stale(&records);
        Some(parent)
    } else {
        None
    };
    let mut settings = Settings::new(parent, cfg.test_env.clone(), &RealDirs::from_env())?;
    settings.record = Some(records.join(std::process::id().to_string()));
    *SETTINGS.write().unwrap() = Some(Arc::new(settings));
    Ok(())
}

/// The git directory of the repository `dir` (or its nearest existing
/// ancestor) is inside, or `None` when it is in none or git cannot tell.
pub fn enclosing_repository(dir: &Path) -> Option<String> {
    let existing = dir.ancestors().find(|d| d.is_dir())?;
    let out = Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .current_dir(existing)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// [`Settings::summary`] of the configured settings, after deleting this
/// process's (now empty) sandbox directory and its record.
pub fn summary() -> Option<String> {
    let settings = current()?;
    settings.finish();
    settings.summary()
}

impl Settings {
    fn finish(&self) {
        self.finished.store(true, Ordering::SeqCst);
        if let Some(root) = self.root() {
            let _ = fs::remove_dir(root);
        }
        if let Some(record) = &self.record {
            let _ = fs::remove_file(record);
        }
    }
}

/// Delete the sandboxes of QS processes that are gone. `records` holds one
/// file per process, named by its id, with the path of its sandbox
/// directory. (Before sandboxes moved out of the repository, each entry was
/// the sandbox directory itself.)
fn remove_stale(records: &Path) {
    let Ok(entries) = fs::read_dir(records) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if pid as u32 == std::process::id() || alive(pid) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            let _ = remove_tree(&path);
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            let root = PathBuf::from(text.trim());
            let named = root
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(ROOT_PREFIX));
            if root.is_absolute() && named {
                let _ = remove_tree(&root);
            }
        }
        let _ = fs::remove_file(&path);
    }
}

fn alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0 checks for the process without signalling it.
        let signalled = unsafe { libc::kill(pid, 0) } == 0;
        signalled || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_real(dir: &Path) -> RealDirs {
        let home = dir.join("real-home");
        fs::create_dir_all(home.join(".config/git")).unwrap();
        fs::write(home.join(".gitconfig"), "[user]\n\tname = Q\n").unwrap();
        fs::write(home.join(".config/git/config"), "[core]\n").unwrap();
        RealDirs {
            git_config: Some(home.join(".config/git/config")),
            mbx_config: None,
            cargo_home: Some(home.join(".cargo")),
            rustup_home: Some(home.join(".rustup")),
            mbx_cache: Some(dir.join("mbx-cache")),
            home: Some(home),
        }
    }

    #[test]
    fn a_test_process_gets_its_own_home_and_tmpdir_which_are_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let real = fake_real(tmp.path());
        let parent = tmp.path().join("sandboxes");
        let env = [("APP_CACHE".to_string(), "{sandbox}/app".to_string())]
            .into_iter()
            .collect();
        let mut settings = Settings::new(Some(parent.clone()), env, &real).unwrap();
        let record = tmp.path().join("state/sandbox/1234");
        settings.record = Some(record.clone());
        let settings = Arc::new(settings);
        let out = tmp.path().join("seen.txt");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!(
            "echo \"$HOME|$TMPDIR|$XDG_CACHE_HOME|$APP_CACHE|$CARGO_HOME|$MBX_CACHE_DIR\" > {}; \
             test -f \"$HOME/.gitconfig\" && test -f \"$XDG_CONFIG_HOME/git/config\" || exit 3; \
             head -c 5000 /dev/zero > \"$TMPDIR/big\"",
            out.display()
        ));
        let sandbox = settings.prepare(&mut cmd).unwrap().expect("sandbox on");
        let dir = sandbox.dir().to_path_buf();
        let root = settings.root().unwrap().to_path_buf();
        assert_eq!(dir.parent(), Some(root.as_path()));
        assert_eq!(root.parent(), Some(parent.as_path()));
        assert!(root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(ROOT_PREFIX));
        assert_eq!(fs::read_to_string(&record).unwrap(), root.to_string_lossy());
        assert!(cmd.status().unwrap().success());
        drop(sandbox);
        let seen = fs::read_to_string(&out).unwrap();
        let fields: Vec<&str> = seen.trim().split('|').collect();
        let d = dir.to_string_lossy();
        assert_eq!(fields[0], format!("{d}/home"));
        assert_eq!(fields[1], format!("{d}/tmp"));
        assert_eq!(fields[2], format!("{d}/home/.cache"));
        assert_eq!(fields[3], format!("{d}/app"));
        assert_eq!(fields[4], real.cargo_home.unwrap().to_string_lossy());
        assert_eq!(fields[5], real.mbx_cache.unwrap().to_string_lossy());
        assert!(!dir.exists(), "the sandbox is deleted");
        settings.finish();
        assert!(!root.exists(), "the run's directory is deleted at the end");
        assert!(!record.exists(), "and so is its record");
        let summary = settings.summary().unwrap();
        assert!(summary.contains("1 test processes"), "{summary}");
        assert!(summary.contains("all deleted"), "{summary}");
        // The two config copies and the 5000-byte file.
        assert!(settings.largest.load(Ordering::Relaxed) >= 5000);
        // A process started after the end does not leave the directory.
        let mut late = Command::new("/bin/true");
        let sandbox = settings.prepare(&mut late).unwrap().unwrap();
        assert!(root.exists());
        drop(sandbox);
        assert!(!root.exists());
    }

    /// A Unix socket's path must fit in about 100 bytes. The mbx cargo shim
    /// binds one in TMPDIR, so a test that runs cargo needs room for it
    /// under the system temporary directory.
    #[cfg(unix)]
    #[test]
    fn a_socket_fits_in_tmpdir_under_the_system_temporary_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = Arc::new(
            Settings::new(
                Some(tmp.path().to_path_buf()),
                BTreeMap::new(),
                &RealDirs::default(),
            )
            .unwrap(),
        );
        let mut cmd = Command::new("/bin/true");
        let sandbox = settings.prepare(&mut cmd).unwrap().unwrap();
        let tmpdir = cmd
            .get_envs()
            .find(|(k, _)| *k == "TMPDIR")
            .and_then(|(_, v)| v)
            .map(PathBuf::from)
            .unwrap();
        let socket = tmpdir.join("mbx-session-OnIIon").join("cache-agent.sock");
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        assert!(sandbox
            .dir()
            .join("tmp/mbx-session-OnIIon/cache-agent.sock")
            .exists());
        drop(sandbox);
        settings.finish();
    }

    fn git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    #[test]
    fn a_directory_under_a_repository_is_seen_as_inside_it() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        // The layout of #14: QS's state under the repository's .git.
        let under_git = repo.join(".git/quicksilver/main/.quicksilver/sandbox");
        let found = enclosing_repository(&under_git).expect("inside");
        assert!(found.ends_with(".git"), "{found}");
        assert!(enclosing_repository(&repo.join("src")).is_some());
        let outside = tmp.path().join("elsewhere");
        fs::create_dir_all(&outside).unwrap();
        assert_eq!(enclosing_repository(&outside), None);
    }

    #[test]
    fn sandboxes_of_finished_runs_are_deleted_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let records = tmp.path().join("records");
        fs::create_dir_all(&records).unwrap();
        // Process ids above Linux's limit (2^22): no process has them.
        let dead = 999_999_000u32;
        let stale = tmp.path().join(format!("{ROOT_PREFIX}stale"));
        fs::create_dir_all(stale.join("0/tmp")).unwrap();
        fs::write(
            records.join(dead.to_string()),
            stale.to_string_lossy().as_bytes(),
        )
        .unwrap();
        // A record naming a directory QS did not make is dropped, the
        // directory kept.
        let foreign = tmp.path().join("not-a-sandbox");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(
            records.join((dead + 1).to_string()),
            foreign.to_string_lossy().as_bytes(),
        )
        .unwrap();
        // The layout before sandboxes left the repository: a directory.
        fs::create_dir_all(records.join((dead + 2).to_string()).join("0/tmp")).unwrap();
        // A running process keeps its record and sandbox.
        let live = tmp.path().join(format!("{ROOT_PREFIX}live"));
        fs::create_dir_all(&live).unwrap();
        fs::write(records.join("1"), live.to_string_lossy().as_bytes()).unwrap();
        remove_stale(&records);
        assert!(!stale.exists());
        assert!(foreign.exists());
        assert!(live.exists());
        let left: Vec<String> = fs::read_dir(&records)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["1".to_string()]);
    }

    #[test]
    fn a_read_only_directory_left_by_a_test_is_still_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = Arc::new(
            Settings::new(
                Some(tmp.path().join("sandbox")),
                BTreeMap::new(),
                &RealDirs::default(),
            )
            .unwrap(),
        );
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("mkdir \"$TMPDIR/ro\" && touch \"$TMPDIR/ro/f\" && chmod 500 \"$TMPDIR/ro\"");
        let sandbox = settings.prepare(&mut cmd).unwrap().unwrap();
        let dir = sandbox.dir().to_path_buf();
        assert!(cmd.status().unwrap().success());
        drop(sandbox);
        assert!(!dir.exists());
        settings.finish();
        assert!(settings.summary().unwrap().contains("all deleted"));
    }

    #[test]
    fn test_env_is_checked_and_applies_without_a_sandbox() {
        let real = RealDirs::default();
        let env = |k: &str, v: &str| [(k.to_string(), v.to_string())].into_iter().collect();
        assert!(Settings::new(None, env("QS_MUTANT", "1"), &real).is_err());
        assert!(Settings::new(None, env("CACHE", "{sandbox}/c"), &real).is_err());
        let settings = Arc::new(Settings::new(None, env("CACHE", "/x"), &real).unwrap());
        let mut cmd = Command::new("/bin/true");
        assert!(settings.prepare(&mut cmd).unwrap().is_none());
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "CACHE" && v == Some("/x".as_ref())));
        assert!(!cmd.get_envs().any(|(k, _)| k == "HOME"));
        assert_eq!(settings.summary(), None);
    }

    #[test]
    fn sizes_are_shown_in_binary_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(44 * 1024 * 1024 * 1024), "44.0 GiB");
    }
}
