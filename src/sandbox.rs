//! A fresh home and temporary directory for each test process QS starts,
//! so that tests, and the mutants they run, cannot write the user's own
//! state: caches under `~/.cache`, config files, files left in `/tmp`.
//!
//! Each process gets `HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`,
//! `XDG_DATA_HOME`, `XDG_STATE_HOME` and `TMPDIR` in its own directory under
//! QS's state, on the same filesystem as the worktrees. The directory is
//! deleted when the process exits. The variables name it through a short
//! symbolic link under the system temporary directory, because a Unix
//! socket's path must fit in about 108 bytes and the state directory can be
//! deep. Tools that tests may run keep working:
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

/// The placeholder in `[test-env]` values for the process's sandbox.
pub const PLACEHOLDER: &str = "{sandbox}";

/// How every test process QS starts is set up.
#[derive(Debug)]
pub struct Settings {
    /// Where this QS process's sandboxes go; `None` when the sandbox is off.
    root: Option<PathBuf>,
    /// Variables every sandboxed process gets, such as the real `CARGO_HOME`.
    fixed: Vec<(String, String)>,
    /// Files copied into each sandbox: (real file, path under the sandbox).
    copies: Vec<(PathBuf, PathBuf)>,
    /// `[test-env]`, with the placeholder still in the values.
    test_env: BTreeMap<String, String>,
    /// A private directory under the system temporary directory for the
    /// short links to sandboxes, made on first use; `None` if it cannot be.
    links: OnceLock<Option<PathBuf>>,
    next: AtomicU64,
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
    /// Settings with the sandbox at `root` (none when off) and `[test-env]`.
    pub fn new(
        root: Option<PathBuf>,
        test_env: BTreeMap<String, String>,
        real: &RealDirs,
    ) -> Result<Settings> {
        for (key, value) in &test_env {
            if key.starts_with("QS_") {
                bail!("[test-env] cannot set {key}: QS_ variables are quicksilver's own");
            }
            if root.is_none() && value.contains(PLACEHOLDER) {
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
            root,
            fixed,
            copies,
            test_env,
            links: OnceLock::new(),
            next: AtomicU64::new(0),
            processes: AtomicU64::new(0),
            largest: AtomicU64::new(0),
            kept: Mutex::new(Vec::new()),
        })
    }

    /// Set up `cmd` to run in a new sandbox, which the returned guard
    /// deletes when dropped. Without a sandbox, only `[test-env]` is set.
    pub fn prepare(self: &Arc<Self>, cmd: &mut Command) -> Result<Option<Sandbox>> {
        let Some(root) = &self.root else {
            for (key, value) in &self.test_env {
                cmd.env(key, value);
            }
            return Ok(None);
        };
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
        let link = self.link_to(&dir, n);
        let shown = link.as_deref().unwrap_or(&dir);
        for (key, path) in sandbox_vars(shown) {
            cmd.env(key, path);
        }
        for (key, value) in &self.fixed {
            cmd.env(key, value);
        }
        let shown_text = shown.to_string_lossy();
        for (key, value) in &self.test_env {
            cmd.env(key, value.replace(PLACEHOLDER, &shown_text));
        }
        Ok(Some(Sandbox {
            settings: Arc::clone(self),
            dir,
            link,
        }))
    }

    /// A shorter path to `dir`: a symbolic link in the private directory
    /// for links. `None` when that is not shorter or cannot be made.
    fn link_to(&self, dir: &Path, n: u64) -> Option<PathBuf> {
        let links = self
            .links
            .get_or_init(|| private_dir(&std::env::temp_dir()))
            .as_ref()?;
        let link = links.join(n.to_string());
        if link.as_os_str().len() >= dir.as_os_str().len() {
            return None;
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir, &link).ok()?;
            Some(link)
        }
        #[cfg(not(unix))]
        {
            None
        }
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
    link: Option<PathBuf>,
}

impl Sandbox {
    /// Where the files are, under QS's state.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The path the process's variables use: the short link, if any.
    pub fn path(&self) -> &Path {
        self.link.as_deref().unwrap_or(&self.dir)
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
        if let Some(link) = &self.link {
            let _ = fs::remove_file(link);
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
        let path = parent.join(format!("qs-sandbox-{name}"));
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
/// this QS process starts, with `[test-env]`. Sandboxes of QS processes
/// that are gone are deleted.
pub fn configure(cfg: &Config, paths: &Paths) -> Result<()> {
    let base = paths.session.join("sandbox");
    let root = cfg
        .sandbox
        .then(|| base.join(std::process::id().to_string()));
    if root.is_some() {
        remove_stale(&base);
    }
    let settings = Settings::new(root, cfg.test_env.clone(), &RealDirs::from_env())?;
    *SETTINGS.write().unwrap() = Some(Arc::new(settings));
    Ok(())
}

/// [`Settings::summary`] of the configured settings, after deleting this
/// process's (now empty) sandbox and link directories.
pub fn summary() -> Option<String> {
    let settings = current()?;
    settings.remove_empty_dirs();
    settings.summary()
}

impl Settings {
    fn remove_empty_dirs(&self) {
        if let Some(root) = &self.root {
            let _ = fs::remove_dir(root);
        }
        if let Some(Some(links)) = self.links.get() {
            let _ = fs::remove_dir(links);
        }
    }
}

fn remove_stale(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
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
        if pid as u32 != std::process::id() && !alive(pid) {
            let _ = remove_tree(&entry.path());
        }
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
        let root = tmp.path().join("sandbox");
        let env = [("APP_CACHE".to_string(), "{sandbox}/app".to_string())]
            .into_iter()
            .collect();
        let settings = Arc::new(Settings::new(Some(root.clone()), env, &real).unwrap());
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
        let shown = sandbox.path().to_path_buf();
        assert!(cmd.status().unwrap().success());
        drop(sandbox);
        let seen = fs::read_to_string(&out).unwrap();
        let fields: Vec<&str> = seen.trim().split('|').collect();
        let d = shown.to_string_lossy();
        assert_eq!(fields[0], format!("{d}/home"));
        assert_eq!(fields[1], format!("{d}/tmp"));
        assert_eq!(fields[2], format!("{d}/home/.cache"));
        assert_eq!(fields[3], format!("{d}/app"));
        assert_eq!(fields[4], real.cargo_home.unwrap().to_string_lossy());
        assert_eq!(fields[5], real.mbx_cache.unwrap().to_string_lossy());
        assert!(!dir.exists(), "the sandbox is deleted");
        assert!(shown.symlink_metadata().is_err(), "the link is deleted");
        settings.remove_empty_dirs();
        let summary = settings.summary().unwrap();
        assert!(summary.contains("1 test processes"), "{summary}");
        assert!(summary.contains("all deleted"), "{summary}");
        // The two config copies and the 5000-byte file.
        assert!(settings.largest.load(Ordering::Relaxed) >= 5000);
    }

    /// A Unix socket's path must fit in about 108 bytes; the short link
    /// keeps sockets in TMPDIR working when the state directory is deep.
    #[cfg(unix)]
    #[test]
    fn sockets_in_tmpdir_work_when_the_state_directory_is_deep() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("d".repeat(60)).join("e".repeat(60));
        let settings =
            Arc::new(Settings::new(Some(root), BTreeMap::new(), &RealDirs::default()).unwrap());
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
        settings.remove_empty_dirs();
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
        settings.remove_empty_dirs();
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
