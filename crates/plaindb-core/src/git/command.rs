use std::ffi::OsString;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::hooks;

pub const GIT_BINARY_ENV: &str = "PLAINDB_GIT";
const REPOSITORY_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];
const FAILING_ASKPASS: &str = "false";
const BATCH_SSH: &str = "ssh -o BatchMode=yes";
const PIPE_GRACE: Duration = Duration::from_secs(2);

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

pub(crate) fn resolve(root: &Path) -> Binary {
    if let Some(program) = hooks::git_program(root) {
        return Binary {
            program,
            source: "env",
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

pub(crate) fn is_allowed(binary: &Binary) -> bool {
    let program = Path::new(&binary.program);
    program.is_absolute() || program.components().count() == 1 && program.file_name().is_some()
}

fn is_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|meta| meta.is_file())
}

pub(crate) fn is_available(binary: &Binary) -> bool {
    if !is_allowed(binary) {
        return false;
    }
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
        || super::open(root).is_ok_and(|repo| {
            repo.config_snapshot()
                .string("core.sshCommand")
                .is_some_and(|value| !value.to_string().trim().is_empty())
        })
}

struct Drain {
    bytes: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Receiver<()>,
}

impl Drain {
    fn start(pipe: Option<impl Read + Send + 'static>) -> Self {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (sender, done) = mpsc::channel();
        let shared = Arc::clone(&bytes);
        thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let mut chunk = [0_u8; 8192];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => shared
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .extend_from_slice(&chunk[..read]),
                    }
                }
            }
            let _ = sender.send(());
        });
        Self { bytes, done }
    }

    fn finish(self, deadline: Instant) -> String {
        let _ = self
            .done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let bytes = self.bytes.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[cfg(unix)]
fn own_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn own_group(_command: &mut Command) {}

#[cfg(unix)]
fn kill_group(child: &mut Child) {
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_group(child: &mut Child) {
    let _ = child.kill();
}

fn command(program: &str, root: &Path, args: &[&str]) -> std::io::Result<Command> {
    let root = std::path::absolute(root)?;
    let mut git_dir = OsString::from("--git-dir=");
    git_dir.push(root.join(".git"));
    let mut work_tree = OsString::from("--work-tree=");
    work_tree.push(&root);
    let mut command = Command::new(program);
    for name in REPOSITORY_ENV {
        command.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_") {
            command.env_remove(name.as_ref());
        }
    }
    command
        .arg(git_dir)
        .arg(work_tree)
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
    if !ssh_is_configured(&root) {
        command.env("GIT_SSH_COMMAND", BATCH_SSH);
    }
    Ok(command)
}

pub(crate) fn run_git(root: &Path, args: &[&str], timeout: Duration) -> std::io::Result<GitRun> {
    let binary = resolve(root);
    if !is_allowed(&binary) {
        return Err(ErrorKind::NotFound.into());
    }
    let mut command = command(&binary.program, root, args)?;
    own_group(&mut command);
    let mut child = command.spawn()?;
    let stdout = Drain::start(child.stdout.take());
    let stderr = Drain::start(child.stderr.take());
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            let drained = Instant::now() + PIPE_GRACE;
            return Ok(GitRun {
                success: status.success(),
                stdout: stdout.finish(drained),
                stderr: stderr.finish(drained),
            });
        }
        if Instant::now() >= deadline {
            kill_group(&mut child);
            let _ = child.wait();
            let drained = Instant::now() + PIPE_GRACE;
            stdout.finish(drained);
            stderr.finish(drained);
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
