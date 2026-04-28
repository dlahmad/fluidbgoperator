use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

pub fn require(name: &str) -> Result<()> {
    let status = Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("missing required command: {name}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("required command {name} exists but returned {status}")
    }
}

pub fn run<I, S>(program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = collect(args);
    let status = Command::new(program)
        .args(&args)
        .status()
        .with_context(|| format!("failed to start {}", format_command(program, &args)))?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "command failed with {status}: {}",
            format_command(program, &args)
        ))
    }
}

pub fn run_in<I, S>(workdir: &Path, program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = collect(args);
    let status = Command::new(program)
        .current_dir(workdir)
        .args(&args)
        .status()
        .with_context(|| format!("failed to start {}", format_command(program, &args)))?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "command failed with {status}: {}",
            format_command(program, &args)
        ))
    }
}

pub fn output<I, S>(program: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = collect(args);
    let output = Command::new(program)
        .args(&args)
        .output()
        .with_context(|| format!("failed to start {}", format_command(program, &args)))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(anyhow!(
            "command failed with {}: {}\n{}",
            output.status,
            format_command(program, &args),
            stderr
        ))
    }
}

pub fn output_allow_failure<I, S>(program: &str, args: I) -> Result<Option<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = collect(args);
    let output = Command::new(program)
        .args(&args)
        .output()
        .with_context(|| format!("failed to start {}", format_command(program, &args)))?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

pub fn spawn_silent(program: &str, args: &[String]) -> Result<std::process::Child> {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start {}", format_command(program, args)))
}

pub fn repo_root() -> Result<std::path::PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("e2e manifest has no parent")?
        .to_path_buf();
    Ok(root)
}

fn collect<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    args.into_iter()
        .map(|arg| arg.as_ref().to_string_lossy().to_string())
        .collect()
}

fn format_command(program: &str, args: &[String]) -> String {
    let mut rendered = String::from(program);
    for arg in args {
        let _ = write!(rendered, " {}", shell_escape(arg));
    }
    rendered
}

fn shell_escape(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_./:=+".contains(ch))
    {
        value.to_string()
    } else {
        format!("{value:?}")
    }
}
