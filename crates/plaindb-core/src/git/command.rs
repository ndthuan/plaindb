use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::StoreConfig;

pub const GIT_BINARY_ENV: &str = "PLAINDB_GIT";
const FAILING_ASKPASS: &str = "false";
const BATCH_SSH: &str = "ssh -o BatchMode=yes";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Binary {
    pub program: String,
    pub source: &'static str,
}

pub(crate) struct GitRun {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

fn default_program() -> &'static str {
    if cfg!(windows) { "git.exe" } else { "git" }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}

pub(crate) fn resolve(config: &StoreConfig) -> Binary {
    if let Some(program) = non_empty(config.git_binary.clone()) {
        return Binary {
            program,
            source: "config",
        };
    }
    if let Some(program) = non_empty(std::env::var(GIT_BINARY_ENV).ok()) {
        return Binary {
            program,
            source: "env",
        };
    }
    Binary {
        program: default_program().to_owned(),
        source: "path",
    }
}

fn is_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|meta| meta.is_file())
}

pub(crate) fn is_available(binary: &Binary) -> bool {
    let program = Path::new(&binary.program);
    if program.components().count() > 1 {
        return is_file(program);
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| is_file(&dir.join(program))))
}

fn ssh_is_configured(root: &Path) -> bool {
    let env_set = ["GIT_SSH_COMMAND", "GIT_SSH"]
        .into_iter()
        .any(|name| non_empty(std::env::var(name).ok()).is_some());
    env_set
        || gix::open(root).is_ok_and(|repo| {
            repo.config_snapshot()
                .string("core.sshCommand")
                .is_some_and(|value| !value.to_string().trim().is_empty())
        })
}

fn drain(mut pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = pipe.as_mut() {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn command(root: &Path, config: &StoreConfig, args: &[&str]) -> Command {
    let mut command = Command::new(resolve(config).program);
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .env("LC_ALL", "C")
        .env("LANGUAGE", "")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", FAILING_ASKPASS)
        .env("SSH_ASKPASS", FAILING_ASKPASS)
        .env("SSH_ASKPASS_REQUIRE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !ssh_is_configured(root) {
        command.env("GIT_SSH_COMMAND", BATCH_SSH);
    }
    command
}

pub(crate) fn run_git(
    root: &Path,
    config: &StoreConfig,
    args: &[&str],
    timeout: Duration,
) -> std::io::Result<GitRun> {
    let mut child = command(root, config, args).spawn()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(GitRun {
                success: status.success(),
                stdout: stdout.join().unwrap_or_default(),
                stderr: stderr.join().unwrap_or_default(),
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(GitRun {
                success: false,
                stdout: String::new(),
                stderr: "the git command timed out".to_owned(),
            });
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) fn rejected_as_behind(porcelain: &str) -> bool {
    porcelain.lines().any(|line| {
        let mut fields = line.splitn(3, '\t');
        let flag = fields.next();
        let summary = fields.nth(1);
        flag == Some("!")
            && summary.is_some_and(|text| {
                text.starts_with("[rejected]")
                    && (text.contains("(fetch first)") || text.contains("(non-fast-forward)"))
            })
    })
}
