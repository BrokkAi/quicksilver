//! Apply test-only settings at nextest's subprocess boundary. Cargo, rustc,
//! nextest's policy engine and setup phase keep their original environment.
use crate::{config::Config, types::Paths};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Wrapper {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    runner: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Specification {
    env: BTreeMap<String, String>,
    parent: Option<PathBuf>,
    records: PathBuf,
    binaries: Vec<PathBuf>,
    wrappers: BTreeMap<String, Option<Wrapper>>,
}

pub(crate) struct Prepared {
    pub path: PathBuf,
    owned: Vec<PathBuf>,
    cleanup: Option<crate::sandbox::Reaper>,
}
impl Drop for Prepared {
    fn drop(&mut self) {
        // Join the owner-side sweep before removing its invocation files.
        drop(self.cleanup.take());
        for path in &self.owned {
            let _ = fs::remove_file(path);
        }
    }
}

fn table(value: &mut toml::Value) -> Result<&mut toml::map::Map<String, toml::Value>> {
    value
        .as_table_mut()
        .context("nextest configuration field must be a table")
}
fn field_table<'a>(value: &'a mut toml::Value, key: &str) -> Result<&'a mut toml::Value> {
    Ok(table(value)?
        .entry(key)
        .or_insert_with(|| toml::Value::Table(Default::default())))
}
fn array(value: &toml::Value) -> Result<Vec<String>> {
    value
        .as_array()
        .context("wrapper argv must be an array")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .context("wrapper argument must be a string")
        })
        .collect()
}
fn argv(value: &toml::Value) -> Result<Vec<String>> {
    match value {
        toml::Value::String(s) => shell_words::split(s).context("parse nextest wrapper command"),
        toml::Value::Array(_) => array(value),
        _ => bail!("nextest wrapper command must be a string or argv array"),
    }
}
fn parse_wrapper(definition: &toml::Value, workspace: &Path, target: &Path) -> Result<Wrapper> {
    let definition = definition
        .as_table()
        .context("nextest wrapper definition must be a table")?;
    let command = definition
        .get("command")
        .context("nextest wrapper needs command")?;
    let mut env = BTreeMap::new();
    let (mut args, relative) = if let Some(spec) = command.as_table() {
        if let Some(vars) = spec.get("env") {
            for (key, value) in vars.as_table().context("wrapper env must be a table")? {
                env.insert(
                    key.clone(),
                    value
                        .as_str()
                        .context("wrapper env value must be a string")?
                        .to_owned(),
                );
            }
        }
        (
            argv(
                spec.get("command-line")
                    .context("wrapper command needs command-line")?,
            )?,
            spec.get("relative-to")
                .and_then(|v| v.as_str())
                .unwrap_or("none"),
        )
    } else {
        (argv(command)?, "none")
    };
    ensure!(!args.is_empty(), "nextest wrapper command is empty");
    match relative {
        "none" => {}
        "target" => args[0] = target.join(&args[0]).to_string_lossy().into_owned(),
        "workspace-root" => args[0] = workspace.join(&args[0]).to_string_lossy().into_owned(),
        other => bail!("unknown wrapper relative-to: {other}"),
    }
    let runner = definition
        .get("target-runner")
        .and_then(|v| v.as_str())
        .unwrap_or("ignore");
    ensure!(
        matches!(
            runner,
            "ignore" | "overrides-wrapper" | "within-wrapper" | "around-wrapper"
        ),
        "unknown wrapper target-runner policy: {runner}"
    );
    Ok(Wrapper {
        argv: args,
        env,
        runner: runner.to_owned(),
    })
}

fn binary_paths(value: &serde_json::Value, paths: &mut Vec<PathBuf>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(path) = map.get("binary-path").and_then(|p| p.as_str()) {
                paths.push(PathBuf::from(path));
            }
            for value in map.values() {
                binary_paths(value, paths);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                binary_paths(value, paths);
            }
        }
        _ => {}
    }
}

/// Keep every original script rule. Existing wrappers are composed, and the
/// fallback is last in the base profile, below inherited project policy.
pub(crate) fn prepare(cfg: &Config, paths: &Paths) -> Result<Prepared> {
    let base = crate::cargo_cmd::write_nextest_config(
        &paths.index,
        &paths.baseline,
        &cfg.nextest_profile,
    )?;
    let mut prepared = Prepared {
        path: base.clone(),
        owned: Vec::new(),
        cleanup: None,
    };
    if cfg.test_env.is_empty() {
        return Ok(prepared);
    }
    let mut config: toml::Value = toml::from_str(&fs::read_to_string(base)?)?;
    let originals = field_table(field_table(&mut config, "scripts")?, "wrapper")?
        .as_table()
        .context("nextest wrapper definitions must be a table")?
        .clone();
    let id = crate::sitemap::random_id();
    let fallback = format!("quicksilver-env-{id}");
    ensure!(
        !originals.contains_key(&fallback),
        "generated nextest wrapper name collision"
    );
    let spec_path = paths.index.join(format!("nextest-env-{id}.json"));
    prepared.path = paths.index.join(format!("nextest-env-{id}.toml"));
    prepared
        .owned
        .extend([spec_path.clone(), prepared.path.clone()]);
    let records = paths.session.join("sandbox");
    let mut specification = Specification {
        env: cfg.test_env.clone(),
        parent: cfg
            .sandbox
            .then(|| cfg.sandbox_dir.clone().unwrap_or_else(std::env::temp_dir)),
        records,
        binaries: Vec::new(),
        wrappers: BTreeMap::new(),
    };
    if !originals.is_empty() {
        let meta =
            crate::cargo_cmd::load_metadata_with_features(&paths.baseline, &cfg.cargo_features)?;
        let mut cmd = crate::cargo_cmd::cargo_command();
        cmd.current_dir(&paths.baseline);
        crate::cargo_cmd::apply_build_env(&mut cmd, cfg);
        cmd.args([
            "nextest",
            "list",
            "--list-type",
            "binaries-only",
            "--message-format",
            "json",
            "--profile",
            &cfg.nextest_profile,
        ]);
        crate::cargo_cmd::apply_package_select(
            &mut cmd,
            &crate::cargo_cmd::test_packages(cfg, &meta),
        );
        cfg.cargo_features.apply(&mut cmd);
        let output = cmd
            .output()
            .context("list nextest binaries for wrapper composition")?;
        ensure!(
            output.status.success(),
            "nextest binaries-only listing failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let binaries: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        binary_paths(&binaries, &mut specification.binaries);
        let target = binaries
            .get("rust-build-meta")
            .and_then(|m| m.get("target-directory"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| meta.target_directory.clone().into_std_path_buf());
        for (name, original) in &originals {
            specification.wrappers.insert(
                name.clone(),
                Some(parse_wrapper(
                    original,
                    meta.workspace_root.as_std_path(),
                    &target,
                )?),
            );
        }
    }
    specification.wrappers.insert(fallback.clone(), None);
    let executable = crate::artifacts::executable()?;
    let definitions = table(field_table(
        field_table(&mut config, "scripts")?,
        "wrapper",
    )?)?;
    for name in specification.wrappers.keys() {
        let definition = definitions
            .entry(name.clone())
            .or_insert_with(|| toml::Value::Table(Default::default()));
        let definition = table(definition)?;
        let args = vec![
            executable.to_string_lossy().into_owned(),
            "__baseline-env".into(),
            "--spec".into(),
            spec_path.to_string_lossy().into_owned(),
            "--wrapper".into(),
            name.clone(),
            "--".into(),
        ];
        definition.insert(
            "command".into(),
            toml::Value::Array(args.into_iter().map(toml::Value::String).collect()),
        );
        definition.insert(
            "target-runner".into(),
            toml::Value::String("within-wrapper".into()),
        );
    }
    let experimental = table(&mut config)?
        .entry("experimental")
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .context("nextest experimental must be an array")?;
    if !experimental
        .iter()
        .any(|v| v.as_str() == Some("wrapper-scripts"))
    {
        experimental.push(toml::Value::String("wrapper-scripts".into()));
    }
    let defaults = field_table(field_table(&mut config, "profile")?, "default")?;
    let rules = table(defaults)?
        .entry("scripts")
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .context("nextest profile scripts must be an array")?;
    rules.push(toml::Value::Table(
        [
            ("filter".into(), toml::Value::String("all()".into())),
            ("run-wrapper".into(), toml::Value::String(fallback.clone())),
            ("list-wrapper".into(), toml::Value::String(fallback)),
        ]
        .into_iter()
        .collect(),
    ));
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&spec_path)?;
    file.write_all(&serde_json::to_vec(&specification)?)?;
    crate::util::write_atomic(&prepared.path, toml::to_string_pretty(&config)?)?;
    if cfg
        .test_env
        .values()
        .any(|v| v.contains(crate::sandbox::PLACEHOLDER))
    {
        prepared.cleanup = Some(crate::sandbox::Reaper::start(
            paths.session.join("sandbox"),
        )?);
    }
    Ok(prepared)
}

fn compose(
    wrapper: Option<&Wrapper>,
    args: &[OsString],
    binaries: &[PathBuf],
) -> Result<(Vec<OsString>, BTreeMap<String, String>)> {
    ensure!(!args.is_empty(), "nextest wrapper received no test command");
    let Some(wrapper) = wrapper else {
        return Ok((args.to_vec(), BTreeMap::new()));
    };
    let original: Vec<_> = wrapper.argv.iter().map(OsString::from).collect();
    if wrapper.runner == "within-wrapper" {
        return Ok((
            original.into_iter().chain(args.iter().cloned()).collect(),
            wrapper.env.clone(),
        ));
    }
    let at = args
        .iter()
        .rposition(|arg| {
            let path = Path::new(arg);
            binaries.iter().any(|known| {
                path == known
                    || path
                        .canonicalize()
                        .ok()
                        .zip(known.canonicalize().ok())
                        .is_some_and(|(a, b)| a == b)
                    || (path.is_absolute() && path.file_name() == known.file_name())
            })
        })
        .context("cannot locate the nextest test binary while composing its target runner")?;
    let (runner, test) = args.split_at(at);
    if runner.is_empty() {
        return Ok((
            original.into_iter().chain(test.iter().cloned()).collect(),
            wrapper.env.clone(),
        ));
    }
    let command = match wrapper.runner.as_str() {
        "ignore" => original.into_iter().chain(test.iter().cloned()).collect(),
        "overrides-wrapper" => return Ok((args.to_vec(), BTreeMap::new())),
        "around-wrapper" => runner
            .iter()
            .cloned()
            .chain(original)
            .chain(test.iter().cloned())
            .collect(),
        _ => unreachable!("validated wrapper policy"),
    };
    Ok((command, wrapper.env.clone()))
}

pub(crate) fn helper(args: crate::cli::BaselineEnvArgs) -> Result<i32> {
    let specification: Specification =
        serde_json::from_slice(&fs::read(&args.spec).context("read nextest test environment")?)?;
    let wrapper = specification
        .wrappers
        .get(&args.wrapper)
        .context("unknown nextest environment wrapper")?;
    let (argv, original_env) = compose(wrapper.as_ref(), &args.args, &specification.binaries)?;
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command.envs(original_env);
    command
        .env_remove("QS_MUTANT")
        .env_remove("QS_RECORD_SITES");
    let settings = crate::sandbox::Settings::for_baseline(
        specification.parent,
        specification.env,
        &specification.records,
    )?;
    let _sandbox = settings.prepare_test_env(&mut command)?;
    use std::os::unix::process::CommandExt;
    Err(command.exec()).context("execute nextest test environment command")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_command_forms_keep_quoting_environment_and_relative_paths() {
        let source: toml::Value = toml::from_str(r#"
[string]
command = "'a path/wrapper' 'two words' escaped\\ space"
[array]
command = ["wrapper", "'literal quotes'", ""]
[table]
command = { command-line = ["wrapper", "--flag"], relative-to = "workspace-root", env = { FLAG = "value" } }
target-runner = "around-wrapper"
[target]
command = { command-line = "bin/wrapper --flag", relative-to = "target" }
"#).unwrap();
        let parse = |key: &str| {
            parse_wrapper(&source[key], Path::new("/workspace"), Path::new("/target")).unwrap()
        };
        assert_eq!(
            parse("string").argv,
            ["a path/wrapper", "two words", "escaped space"]
        );
        assert_eq!(parse("array").argv, ["wrapper", "'literal quotes'", ""]);
        let table = parse("table");
        assert_eq!(table.argv, ["/workspace/wrapper", "--flag"]);
        assert_eq!(table.env["FLAG"], "value");
        assert_eq!(table.runner, "around-wrapper");
        assert_eq!(parse("target").argv, ["/target/bin/wrapper", "--flag"]);
    }
}
