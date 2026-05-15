use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, Semaphore};

use nichy::TypeLayout;

use crate::runner::Job;

#[derive(Serialize)]
struct WorkerRequest<'a> {
    kind: &'a str,
    input: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
}

#[derive(Deserialize)]
struct WorkerResponse {
    #[serde(default)]
    layouts: Option<Vec<TypeLayout>>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct ReadyLine {
    pid: u32,
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    jobs_done: usize,
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
            set.spawn(async move { spawn_worker(&bin).await });
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

    /// Submit a job. Returns layouts on success; on failure returns raw error
    /// text with the right HTTP status. The caller is responsible for any
    /// presentation-layer cleanup (e.g. rewriting rustc line numbers).
    pub async fn submit(
        &self,
        job: Job<'_>,
        target: Option<&str>,
        timeout_secs: f64,
    ) -> Result<Vec<TypeLayout>, (StatusCode, String)> {
        let permit = self.available.acquire().await.expect("semaphore closed");
        let mut worker = self
            .free
            .lock()
            .await
            .pop_front()
            .expect("permit granted but no worker free");
        // The permit's life is now tied to the worker; we'll re-add it when
        // we return a worker to the free list or when respawn replaces it.
        permit.forget();

        let (kind, input) = match job {
            Job::TypeExpr(s) => ("type", s),
            Job::Snippet(s) => ("snippet", s),
        };

        let req = WorkerRequest {
            kind,
            input,
            target,
        };

        let request_json = serde_json::to_string(&req)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("encode: {e}")))?;

        let outcome = tokio::time::timeout(
            Duration::from_secs_f64(timeout_secs + 1.0),
            run_one(&mut worker, &request_json),
        )
        .await;

        let (result, recycle) = match outcome {
            Ok(Ok(resp)) => {
                worker.jobs_done += 1;
                let recycle = worker.jobs_done >= self.max_jobs_per_worker;
                match resp.layouts {
                    Some(layouts) if !layouts.is_empty() => (Ok(layouts), recycle),
                    _ => {
                        let raw = resp.error.unwrap_or_else(|| "unknown error".into());
                        (Err((StatusCode::UNPROCESSABLE_ENTITY, raw)), recycle)
                    }
                }
            }
            Ok(Err(e)) => (
                Err((StatusCode::BAD_GATEWAY, format!("worker io error: {e}"))),
                true,
            ),
            Err(_) => (
                Err((
                    StatusCode::GATEWAY_TIMEOUT,
                    format!("analysis exceeded {timeout_secs}s timeout"),
                )),
                true,
            ),
        };

        if recycle {
            self.replace_worker(worker).await;
        } else {
            self.free.lock().await.push_back(worker);
            self.available.add_permits(1);
        }

        result
    }

    async fn replace_worker(&self, mut worker: Worker) {
        let _ = worker.child.start_kill();
        drop(worker);

        // Serialize respawns so a burst of failures doesn't fork-bomb us.
        let _spawn_guard = self.spawn_lock.lock().await;
        match spawn_worker(&self.bin).await {
            Ok(new_w) => {
                self.free.lock().await.push_back(new_w);
                self.available.add_permits(1);
            }
            Err(e) => {
                eprintln!("warning: nichy worker respawn failed: {e}");
                // Pool shrinks by one. The permit is not returned, so
                // available semaphore reflects the new total.
            }
        }
    }
}

async fn spawn_worker(bin: &std::path::Path) -> std::io::Result<Worker> {
    let mut cmd = Command::new(bin);
    // stderr is null because the worker captures rustc diagnostics
    // and returns them in the JSON response.
    cmd.arg("--serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    #[cfg(unix)]
    // SAFETY: pre_exec runs in the forked child between fork() and execve();
    // only async-signal-safe operations are permitted. setrlimit is on the
    // safe list per signal(7).
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
    eprintln!("nichy worker ready: pid={}", ready.pid);

    Ok(Worker {
        child,
        stdin,
        stdout,
        jobs_done: 0,
    })
}

async fn run_one(worker: &mut Worker, request_json: &str) -> std::io::Result<WorkerResponse> {
    worker.stdin.write_all(request_json.as_bytes()).await?;
    worker.stdin.write_all(b"\n").await?;
    worker.stdin.flush().await?;

    let mut line = String::new();
    let n = worker.stdout.read_line(&mut line).await?;
    if n == 0 {
        return Err(std::io::Error::other("worker closed stdout"));
    }
    serde_json::from_str(line.trim())
        .map_err(|e| std::io::Error::other(format!("bad response: {e}: {line:?}")))
}
