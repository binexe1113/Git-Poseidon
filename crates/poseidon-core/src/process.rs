use crate::domain::{AppError, ErrorCode, Result};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::watch,
    time::timeout,
};

const MAX_STDOUT: usize = 8 * 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct GitProcess {
    pub(crate) executable: PathBuf,
}

/// Raw output is deliberately neither Debug nor Serialize: it may contain secrets.
pub struct Output {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Time spent in Command::spawn (not the complete OS/loader startup cost).
    pub spawn_duration: Duration,
    /// Spawn, process completion and concurrent pipe draining; excludes parsing.
    pub duration: Duration,
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub timeout: Duration,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: READ_TIMEOUT,
            stdout_bytes: MAX_STDOUT,
            stderr_bytes: MAX_STDERR,
        }
    }
}

/// Retains cancellation even when requested before the runner subscribes.
#[derive(Clone)]
pub struct Cancellation(watch::Sender<bool>);
impl Default for Cancellation {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}
impl Cancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    pub(crate) async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|value| *value).await;
    }
}

// A separate process group also terminates descendants holding pipe handles.
// Drop covers cancellation by dropping the execution future, not just our token.
#[cfg(unix)]
struct ProcessGroup(i32);
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: kill has no pointer arguments; this is the child's dedicated group.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

impl GitProcess {
    pub fn discover() -> Result<Self> {
        let paths = std::env::var_os("PATH").unwrap_or_default();
        let binary = if cfg!(windows) { "git.exe" } else { "git" };
        for dir in std::env::split_paths(&paths) {
            if !dir.is_absolute() {
                continue;
            }
            let path = dir.join(binary);
            if path.is_file() {
                if let Ok(executable) = path.canonicalize() {
                    return Ok(Self { executable });
                }
            }
        }
        Err(AppError::new(
            ErrorCode::GitUnavailable,
            "Git não encontrado no PATH. Instale o Git e reinicie o aplicativo.",
        ))
    }

    /// Backend-only structured argv. Never expose this method as a Tauri command.
    pub async fn run(&self, cwd: &Path, args: &[&str]) -> Result<Output> {
        self.run_with(cwd, args, Limits::default(), &Cancellation::default())
            .await
    }

    /// Arguments are bytes-preserving (`OsStr`) so non-UTF-8 paths survive as pathspecs.
    pub async fn run_with<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
        limits: Limits,
        cancellation: &Cancellation,
    ) -> Result<Output> {
        execute(self.command(cwd, args), limits, cancellation).await
    }

    /// Like `run_with`, plus configuration passed through `GIT_CONFIG_COUNT`/`KEY_n`/`VALUE_n`:
    /// keys may contain `=` or arbitrary bytes, which `-c key=value` cannot express.
    pub async fn run_configured<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
        config: &[(OsString, OsString)],
        limits: Limits,
        cancellation: &Cancellation,
    ) -> Result<Output> {
        self.run_input(cwd, args, config, None, limits, cancellation)
            .await
    }

    /// `run_configured` with bytes written to stdin (NUL-delimited paths or index
    /// records), so no path ever becomes an argument.
    pub async fn run_input<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
        config: &[(OsString, OsString)],
        input: Option<&[u8]>,
        limits: Limits,
        cancellation: &Cancellation,
    ) -> Result<Output> {
        self.run_input_env(cwd, args, config, input, limits, cancellation, &[])
            .await
    }

    /// Backend-controlled environment for the guarded commit; never exposed through IPC.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_input_env<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
        config: &[(OsString, OsString)],
        input: Option<&[u8]>,
        limits: Limits,
        cancellation: &Cancellation,
        env: &[(OsString, OsString)],
    ) -> Result<Output> {
        let mut command = self.command(cwd, args);
        command.envs(env.iter().cloned());
        command.env("GIT_CONFIG_COUNT", config.len().to_string());
        for (i, (key, value)) in config.iter().enumerate() {
            command.env(format!("GIT_CONFIG_KEY_{i}"), key);
            command.env(format!("GIT_CONFIG_VALUE_{i}"), value);
        }
        execute_input(command, limits, cancellation, input).await
    }

    fn command<S: AsRef<OsStr>>(&self, cwd: &Path, args: &[S]) -> Command {
        let mut command = Command::new(&self.executable);
        // Ignore inherited variables that can redirect Git or inject configuration.
        for (name, _) in std::env::vars_os() {
            if name
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(name);
            }
        }
        for name in [
            "ENV",
            "BASH_ENV",
            "CDPATH",
            "GCONV_PATH",
            "LOCPATH",
            "NLSPATH",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "LD_AUDIT",
            "DYLD_INSERT_LIBRARIES",
            "DYLD_LIBRARY_PATH",
            "DYLD_FRAMEWORK_PATH",
        ] {
            command.env_remove(name);
        }
        let path = std::env::join_paths(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .filter(|p| p.is_absolute()),
        )
        .unwrap_or_default();
        command
            .env("PATH", path)
            .env_remove("ENV")
            .env_remove("BASH_ENV")
            .env_remove("CDPATH")
            .current_dir(cwd)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_ALLOW_PROTOCOL", "")
            .env("GIT_PROTOCOL_FROM_USER", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .args([
                "--no-pager",
                "--no-optional-locks",
                "--no-replace-objects",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
                "-c",
                "maintenance.auto=false",
                "-c",
                "gc.auto=0",
                "-c",
                "log.showSignature=false",
                "-c",
                "color.ui=false",
                "-c",
                "i18n.logOutputEncoding=UTF-8",
                "-c",
                "protocol.allow=never",
                "-c",
                "core.quotePath=true",
            ]);
        // An explicit flag fails closed if an unexpected older binary replaces Git.
        // --version must remain usable so installation() can report UnsupportedGit.
        if !(args.len() == 1 && args[0].as_ref() == "--version") {
            command.arg("--no-lazy-fetch");
        }
        command.args(args);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW, not a shell.
        command
    }

    /// Bounded binary records for the fixed ten-field Git log protocol.
    pub(crate) fn history_stream(
        &self,
        cwd: &Path,
        args: &[&str],
        input: Vec<u8>,
    ) -> Result<GitStream> {
        let mut command = self.command(cwd, args);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                AppError::new(
                    ErrorCode::GitUnavailable,
                    "Não foi possível iniciar a travessia Git.",
                )
            })?;
        #[cfg(unix)]
        let group = ProcessGroup(child.id().ok_or_else(AppError::protocol)? as i32);
        let stdout = child.stdout.take().ok_or_else(AppError::protocol)?;
        let stderr = child.stderr.take().ok_or_else(AppError::protocol)?;
        let mut stdin = child.stdin.take().ok_or_else(AppError::protocol)?;
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let task = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::with_capacity(64 * 1024, stdout);
            let result: Result<_> = tokio::try_join!(
                async {
                    while let Some(record) = log_record(&mut reader).await? {
                        timeout(Duration::from_secs(60), sender.send(Ok(record)))
                            .await
                            .map_err(|_| {
                                AppError::new(
                                    ErrorCode::Timeout,
                                    "Travessia inativa. Atualize o histórico.",
                                )
                            })?
                            .map_err(|_| {
                                AppError::new(ErrorCode::Cancelled, "Travessia encerrada.")
                            })?;
                    }
                    Ok(())
                },
                read_limited(stderr, MAX_STDERR),
                async {
                    use tokio::io::AsyncWriteExt;
                    stdin
                        .write_all(&input)
                        .await
                        .map_err(|_| AppError::protocol())?;
                    drop(stdin);
                    Ok(())
                },
                async {
                    let status = child.wait().await.map_err(|_| AppError::protocol())?;
                    if !status.success() {
                        return Err(AppError::new(
                            ErrorCode::CommandFailed,
                            "Não foi possível percorrer o histórico Git.",
                        ));
                    }
                    Ok(())
                }
            );
            #[cfg(unix)]
            drop(group);
            if let Err(error) = result {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = sender.send(Err(error)).await;
            }
        });
        Ok(GitStream { receiver, task })
    }
}

pub(crate) struct GitStream {
    receiver: tokio::sync::mpsc::Receiver<Result<Vec<u8>>>,
    task: tokio::task::JoinHandle<()>,
}
impl GitStream {
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        self.receiver.recv().await.transpose()
    }
}
impl Drop for GitStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn log_record(
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
) -> Result<Option<Vec<u8>>> {
    use tokio::io::AsyncBufReadExt;
    let mut record = Vec::with_capacity(512);
    for field in 0..10 {
        loop {
            let bytes = reader.fill_buf().await.map_err(|_| AppError::protocol())?;
            if bytes.is_empty() {
                return if field == 0 && record.is_empty() {
                    Ok(None)
                } else {
                    Err(AppError::protocol())
                };
            }
            let end = bytes.iter().position(|b| *b == 0);
            let count = end.map_or(bytes.len(), |i| i + 1);
            if record.len() + count > 256 * 1024 {
                return Err(AppError::new(
                    ErrorCode::OutputLimit,
                    "Metadados de um commit excedem 256 KiB.",
                ));
            }
            record.extend_from_slice(&bytes[..count]);
            reader.consume(count);
            if end.is_some() {
                break;
            }
        }
    }
    Ok(Some(record))
}

async fn read_limited(mut pipe: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    (&mut pipe)
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| {
            AppError::new(
                ErrorCode::CommandFailed,
                "Não foi possível ler a resposta do Git.",
            )
        })?;
    if bytes.len() > limit {
        return Err(AppError::new(
            ErrorCode::OutputLimit,
            "A resposta do Git excedeu o limite de dados. Confira o estado antes de repetir uma escrita.",
        ));
    }
    Ok(bytes)
}

async fn execute(command: Command, limits: Limits, cancellation: &Cancellation) -> Result<Output> {
    execute_input(command, limits, cancellation, None).await
}

async fn execute_input(
    mut command: Command,
    limits: Limits,
    cancellation: &Cancellation,
    input: Option<&[u8]>,
) -> Result<Output> {
    if *cancellation.0.borrow() {
        return Err(AppError::new(ErrorCode::Cancelled, "Consulta cancelada."));
    }
    #[cfg(unix)]
    command.process_group(0);
    let start = Instant::now();
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            AppError::new(
                ErrorCode::GitUnavailable,
                "Não foi possível iniciar o executável Git.",
            )
        })?;
    let spawn_duration = start.elapsed();
    #[cfg(unix)]
    let group = ProcessGroup(child.id().ok_or_else(AppError::protocol)? as i32);
    let stdout = child.stdout.take().ok_or_else(AppError::protocol)?;
    let stderr = child.stderr.take().ok_or_else(AppError::protocol)?;
    let stdin = child.stdin.take();
    // Written while the pipes drain; closing it signals the end of the input. A
    // process that exits early only makes the write fail, which is not an error here.
    let feed = async move {
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            let _ = stdin.write_all(input).await;
        }
        Ok::<(), AppError>(())
    };
    let result = tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(AppError::new(ErrorCode::Cancelled, "Consulta cancelada.")),
        result = timeout(limits.timeout, async {
            let status = async {
                child.wait().await.map_err(|_| AppError::new(ErrorCode::CommandFailed, "Não foi possível aguardar o Git."))
            };
            let (status, stdout, stderr, ()) = tokio::try_join!(
                status, read_limited(stdout, limits.stdout_bytes), read_limited(stderr, limits.stderr_bytes), feed
            )?;
            Ok(Output { success: status.success(), code: status.code(), stdout, stderr, spawn_duration, duration: start.elapsed() })
        }) => result.unwrap_or_else(|_| Err(AppError::new(ErrorCode::Timeout, "A consulta ao Git excedeu o prazo configurado."))),
    };
    // Also kill descendants after the immediate child exits or a pipe overflows.
    #[cfg(unix)]
    drop(group);
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_caps_output() {
        assert!(matches!(
            read_limited(&b"abcd"[..], 3).await.unwrap_err().code,
            ErrorCode::OutputLimit
        ));
    }

    #[tokio::test]
    async fn streaming_log_rejects_truncation_and_oversized_records() {
        let truncated = b"a\0b\0";
        assert!(matches!(
            log_record(&mut &truncated[..]).await.err().unwrap().code,
            ErrorCode::InvalidOutput
        ));
        let huge = vec![b'x'; 256 * 1024 + 1];
        assert!(matches!(
            log_record(&mut &huge[..]).await.err().unwrap().code,
            ErrorCode::OutputLimit
        ));
    }

    #[test]
    fn child_process_fixture() {
        match std::env::var("POSEIDON_PROCESS_FIXTURE").as_deref() {
            Ok("sleep") => std::thread::sleep(Duration::from_secs(30)),
            #[cfg(target_os = "linux")]
            Ok(mode @ ("tree-wait" | "tree-exit")) => {
                let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "process::tests::child_process_fixture",
                        "--nocapture",
                    ])
                    .env("POSEIDON_PROCESS_FIXTURE", "sleep")
                    .spawn()
                    .unwrap();
                std::fs::write(
                    std::env::var_os("POSEIDON_PID_FILE").unwrap(),
                    format!("{} {}", std::process::id(), child.id()),
                )
                .unwrap();
                if mode == "tree-exit" {
                    std::process::exit(0);
                }
                child.wait().unwrap();
            }
            Ok("failure") => {
                use std::io::Write;
                std::io::stdout().write_all(b"expected stdout").unwrap();
                std::io::stderr().write_all(b"secret stderr").unwrap();
                std::process::exit(7);
            }
            Ok("stdout-flood") => {
                use std::io::Write;
                let _ = std::io::stdout().write_all(&vec![b'x'; MAX_STDOUT + 4096]);
            }
            Ok("flood") => {
                use std::io::Write;
                let bytes = vec![b'x'; MAX_STDERR + 4096];
                let _ = std::io::stderr().write_all(&bytes);
            }
            _ => {}
        }
    }

    fn helper(mode: &str) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "process::tests::child_process_fixture",
            "--nocapture",
        ])
        .env("POSEIDON_PROCESS_FIXTURE", mode);
        cmd
    }

    #[tokio::test]
    async fn timeout_kills_and_reaps_child() {
        let error = execute(
            helper("sleep"),
            Limits {
                timeout: Duration::from_millis(100),
                ..Limits::default()
            },
            &Cancellation::default(),
        )
        .await
        .err()
        .unwrap();
        assert!(matches!(error.code, ErrorCode::Timeout));
    }

    #[tokio::test]
    async fn exit_code_and_both_pipes_are_preserved() {
        let output = execute(
            helper("failure"),
            Limits::default(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert!(!output.success);
        assert_eq!(output.code, Some(7));
        assert!(output.stdout.ends_with(b"expected stdout"));
        assert_eq!(output.stderr, b"secret stderr");
        assert!(output.duration >= output.spawn_duration);
    }

    #[tokio::test]
    async fn cancellation_before_and_during_execution() {
        let token = Cancellation::default();
        token.cancel();
        assert!(matches!(
            execute(helper("sleep"), Limits::default(), &token)
                .await
                .err()
                .unwrap()
                .code,
            ErrorCode::Cancelled
        ));
        let token = Cancellation::default();
        let cancel = token.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        });
        let result = timeout(
            Duration::from_secs(2),
            execute(helper("sleep"), Limits::default(), &token),
        )
        .await
        .unwrap();
        assert!(matches!(result.err().unwrap().code, ErrorCode::Cancelled));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn stdout_flood_is_bounded() {
        assert!(matches!(
            execute(
                helper("stdout-flood"),
                Limits::default(),
                &Cancellation::default()
            )
            .await
            .err()
            .unwrap()
            .code,
            ErrorCode::OutputLimit
        ));
    }

    #[cfg(target_os = "linux")]
    async fn pids_from_file(path: &Path) -> Vec<u32> {
        for _ in 0..200 {
            if let Ok(value) = std::fs::read_to_string(path) {
                if value.split_whitespace().count() == 2 {
                    return value
                        .split_whitespace()
                        .map(|v| v.parse().unwrap())
                        .collect();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("fixture child did not start");
    }

    #[cfg(target_os = "linux")]
    async fn assert_no_live_processes(pids: Vec<u32>) {
        for _ in 0..100 {
            let live = pids.iter().any(|pid| {
                std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                    !stat
                        .rsplit(')')
                        .next()
                        .unwrap()
                        .trim_start()
                        .starts_with('Z')
                })
            });
            if !live {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("runner left live fixture processes: {pids:?}");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancellation_and_dropped_future_kill_process_group() {
        for abort_future in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let file = temp.path().join("pids");
            let mut cmd = helper("tree-wait");
            cmd.env("POSEIDON_PID_FILE", &file);
            let token = Cancellation::default();
            let cancel = token.clone();
            let task = tokio::spawn(async move { execute(cmd, Limits::default(), &token).await });
            let pids = pids_from_file(&file).await;
            if abort_future {
                task.abort();
                assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            } else {
                cancel.cancel();
                assert!(matches!(
                    task.await.unwrap().err().unwrap().code,
                    ErrorCode::Cancelled
                ));
            }
            assert_no_live_processes(pids).await;
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn timeout_kills_descendant_holding_pipes_after_parent_exit() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("pids");
        let mut cmd = helper("tree-exit");
        cmd.env("POSEIDON_PID_FILE", &file);
        let token = Cancellation::default();
        let result = execute(
            cmd,
            Limits {
                timeout: Duration::from_millis(300),
                ..Limits::default()
            },
            &token,
        )
        .await;
        assert!(matches!(result.err().unwrap().code, ErrorCode::Timeout));
        assert_no_live_processes(pids_from_file(&file).await).await;
    }

    #[tokio::test]
    async fn stderr_flood_is_bounded() {
        let error = execute(helper("flood"), Limits::default(), &Cancellation::default())
            .await
            .err()
            .unwrap();
        assert!(matches!(error.code, ErrorCode::OutputLimit));
    }
}
