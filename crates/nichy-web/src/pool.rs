use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, Semaphore};

use nichy::{JobKind, TypeLayout, WorkerRequest, WorkerResponse};

use crate::runner::Job;

#[derive(Deserialize)]
struct ReadyLine {
    pid: u32,
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pid: u32,
    jobs_done: usize,
}

enum AttemptOutcome {
    Final(Result<Vec<TypeLayout>, (StatusCode, String)>),
    Retryable { message: String, reason: String },
}

pub struct WorkerPool {
    bin: PathBuf,
    free: Mutex<VecDeque<Worker>>,
    available: Semaphore,
    max_jobs_per_worker: usize,
    spawn_lock: Mutex<()>,
}

pub struct PoolConfig {
    pub bin: PathBuf,
    pub workers: usize,
    pub max_jobs_per_worker: usize,
}

impl WorkerPool {
    pub async fn new(cfg: PoolConfig) -> Arc<Self> {
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..cfg.workers {
            let bin = cfg.bin.clone();
            set.spawn(async move { Worker::spawn(&bin).await });
        }
        let mut free = VecDeque::with_capacity(cfg.workers);
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(Ok(w)) => free.push_back(w),
                Ok(Err(e)) => eprintln!("warning: nichy worker failed to spawn: {e}"),
                Err(e) => eprintln!("warning: nichy worker spawn task panicked: {e}"),
            }
        }
        if free.is_empty() {
            panic!(
                "pool: no nichy workers could be spawned (bin={})",
                cfg.bin.display()
            );
        }
        let n = free.len();
        eprintln!("nichy worker pool: {n} live");
        Arc::new(Self {
            bin: cfg.bin,
            free: Mutex::new(free),
            available: Semaphore::new(n),
            max_jobs_per_worker: cfg.max_jobs_per_worker,
            spawn_lock: Mutex::new(()),
        })
    }

    /// Submit a job. Transparently retries transient worker failures (infra
    /// errors, IO errors) on a fresh worker up to `MAX_ATTEMPTS` total.
    /// `UserError` and timeouts are returned directly.
    pub async fn submit(
        &self,
        job: Job<'_>,
        target: Option<&str>,
        timeout_secs: f64,
    ) -> Result<Vec<TypeLayout>, (StatusCode, String)> {
        const MAX_ATTEMPTS: u32 = 2;

        let (kind, input) = match job {
            Job::TypeExpr(s) => (JobKind::Type, s),
            Job::Snippet(s) => (JobKind::Snippet, s),
        };
        let req = WorkerRequest::new(kind, input, target);
        let request_json = serde_json::to_string(&req)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("encode: {e}")))?;

        let mut last_err: Option<(String, String)> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            match self.attempt_once(&request_json, timeout_secs).await {
                AttemptOutcome::Final(r) => return r,
                AttemptOutcome::Retryable { message, reason } => {
                    if attempt < MAX_ATTEMPTS {
                        eprintln!("pool: retry {attempt}/{MAX_ATTEMPTS} ({reason})");
                    }
                    last_err = Some((message, reason));
                }
            }
        }
        let (message, reason) = last_err.expect("retry loop populates last_err");
        Err((
            StatusCode::BAD_GATEWAY,
            format!("{message} (giving up after {MAX_ATTEMPTS} attempts, last={reason})"),
        ))
    }

    async fn attempt_once(&self, request_json: &str, timeout_secs: f64) -> AttemptOutcome {
        let permit = self.available.acquire().await.expect("semaphore closed");
        let mut worker = self
            .free
            .lock()
            .await
            .pop_front()
            .expect("permit granted but no worker free");
        // Permit is now tied to the worker; re-added when the worker rejoins
        // the free list or when a respawn replaces it.
        permit.forget();

        let result = tokio::time::timeout(
            Duration::from_secs_f64(timeout_secs + 1.0),
            worker.run_one(request_json),
        )
        .await;

        let outcome = worker.interpret(result, timeout_secs);
        let recycle = matches!(outcome, AttemptOutcome::Retryable { .. })
            || worker.jobs_done >= self.max_jobs_per_worker;

        if recycle {
            self.replace_worker(worker).await;
        } else {
            self.free.lock().await.push_back(worker);
            self.available.add_permits(1);
        }
        outcome
    }

    async fn replace_worker(&self, mut worker: Worker) {
        let _ = worker.child.start_kill();
        drop(worker);

        // Serialize respawns so a burst of failures doesn't fork-bomb us.
        let _spawn_guard = self.spawn_lock.lock().await;
        match Worker::spawn(&self.bin).await {
            Ok(w) => {
                self.free.lock().await.push_back(w);
                self.available.add_permits(1);
            }
            Err(e) => {
                eprintln!("warning: nichy worker respawn failed: {e}");
                // Pool shrinks by one; the missing permit reflects the new
                // capacity until something else respawns successfully.
            }
        }
    }
}

impl Worker {
    async fn spawn(bin: &std::path::Path) -> std::io::Result<Self> {
        let mut cmd = Command::new(bin);
        // stderr → null because the worker captures rustc diagnostics into
        // its JSON response. MALLOC_ARENA_MAX=2 caps glibc per-thread arenas
        // (~64 MiB virtual each, never freed); without it the worker
        // exhausts RLIMIT_AS around the 25th rustc call and rustc starts
        // failing to mmap rmeta metadata.
        cmd.arg("--serve")
            .env("MALLOC_ARENA_MAX", "2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        #[cfg(unix)]
        // SAFETY: pre_exec runs in the forked child between fork() and
        // execve(); only async-signal-safe operations are permitted.
        // setrlimit is on the safe list per signal(7).
        unsafe {
            cmd.pre_exec(crate::runner::apply_persistent_sandbox);
        }

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("worker stdin missing"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("worker stdout missing"))?;
        let mut stdout = BufReader::new(stdout);

        let mut ready_line = String::new();
        let n = stdout.read_line(&mut ready_line).await?;
        if n == 0 {
            return Err(std::io::Error::other("worker died before ready"));
        }
        let ready: ReadyLine = serde_json::from_str(ready_line.trim())
            .map_err(|e| std::io::Error::other(format!("bad ready line: {e}: {ready_line:?}")))?;

        let mut worker = Self {
            child,
            stdin,
            stdout,
            pid: ready.pid,
            jobs_done: 0,
        };

        worker
            .smoke_probe()
            .await
            .map_err(|e| std::io::Error::other(format!("smoke probe failed: {e}")))?;

        eprintln!("nichy worker ready: pid={}", ready.pid);
        Ok(worker)
    }

    // Catches workers born in a degenerate state (poisoned from job 1).
    // Counted as one job so the recycle budget reflects it.
    async fn smoke_probe(&mut self) -> Result<(), String> {
        let probe = WorkerRequest::new(JobKind::Type, "u8", None);
        let probe_json = serde_json::to_string(&probe).expect("encode probe");
        let resp = tokio::time::timeout(Duration::from_secs(5), self.run_one(&probe_json))
            .await
            .map_err(|_| "probe timed out".to_string())?
            .map_err(|e| format!("probe io: {e}"))?;
        self.jobs_done = 1;
        match resp {
            WorkerResponse::Ok { .. } => Ok(()),
            WorkerResponse::UserError { message } => Err(format!("probe user_error: {message}")),
            WorkerResponse::InfraError { reason, .. } => Err(format!("probe infra ({reason})")),
        }
    }

    fn interpret(
        &mut self,
        result: Result<std::io::Result<WorkerResponse>, tokio::time::error::Elapsed>,
        timeout_secs: f64,
    ) -> AttemptOutcome {
        match result {
            Ok(Ok(resp)) => {
                self.jobs_done += 1;
                match resp {
                    WorkerResponse::Ok { layouts } => AttemptOutcome::Final(Ok(layouts)),
                    WorkerResponse::UserError { message } => {
                        AttemptOutcome::Final(Err((StatusCode::UNPROCESSABLE_ENTITY, message)))
                    }
                    WorkerResponse::InfraError { message, reason } => {
                        eprintln!(
                            "pool: infra error pid={} reason={reason} jobs_done={}",
                            self.pid, self.jobs_done,
                        );
                        AttemptOutcome::Retryable {
                            message: format!("worker infra error ({reason}): {message}"),
                            reason: format!("infra:{reason}"),
                        }
                    }
                }
            }
            Ok(Err(e)) => AttemptOutcome::Retryable {
                message: format!("worker io error: {e}"),
                reason: "io".into(),
            },
            // Timeouts are not retried: a slow input rarely gets faster on a
            // second pass, and retrying doubles the user's latency.
            Err(_) => AttemptOutcome::Final(Err((
                StatusCode::GATEWAY_TIMEOUT,
                format!("analysis exceeded {timeout_secs}s timeout"),
            ))),
        }
    }

    async fn run_one(&mut self, request_json: &str) -> std::io::Result<WorkerResponse> {
        self.stdin.write_all(request_json.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;

        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).await?;
        if n == 0 {
            return Err(std::io::Error::other("worker closed stdout"));
        }
        serde_json::from_str(line.trim())
            .map_err(|e| std::io::Error::other(format!("bad response: {e}: {line:?}")))
    }
}
