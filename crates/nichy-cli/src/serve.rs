use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use nichy::TypeLayout;

#[derive(Deserialize)]
struct Request {
    kind: String,
    input: String,
    #[serde(default)]
    target: Option<String>,
}

#[derive(Serialize)]
struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    layouts: Option<Vec<TypeLayout>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct Ready {
    ready: bool,
    pid: u32,
}

pub fn run() -> ! {
    // Announce readiness so the parent knows the worker is up before the
    // first real request lands.
    let ready = serde_json::to_string(&Ready {
        ready: true,
        pid: std::process::id(),
    })
    .unwrap();
    {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{ready}");
        let _ = out.flush();
    }

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => std::process::exit(0),
            Ok(_) => {}
            Err(_) => std::process::exit(1),
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let resp = handle_one(trimmed);
        let json = serde_json::to_string(&resp).expect("serialize response");
        let mut out = std::io::stdout().lock();
        if writeln!(out, "{json}").is_err() || out.flush().is_err() {
            std::process::exit(1);
        }
    }
}

fn handle_one(line: &str) -> Response {
    let req: Request = match serde_json::from_str::<Request>(line) {
        Ok(r) => r,
        Err(e) => {
            return Response {
                layouts: None,
                error: Some(format!("bad request: {e}")),
            };
        }
    };

    let target = req.target.as_deref();

    let (result, captured) = capture_stderr(|| match req.kind.as_str() {
        "type" => nichy_rustc::analyze_type_expr(&req.input, None, target),
        "snippet" => nichy_rustc::analyze_snippet(&req.input, None, target),
        "file" => nichy_rustc::analyze_file(Path::new(&req.input), None, target),
        other => Err(format!("unknown kind: {other}")),
    });

    match result {
        Ok(layouts) if !layouts.is_empty() => Response {
            layouts: Some(layouts),
            error: None,
        },
        Ok(_) => Response {
            layouts: None,
            error: Some(if captured.trim().is_empty() {
                "no types analyzed".into()
            } else {
                captured
            }),
        },
        Err(e) => Response {
            layouts: None,
            error: Some(if captured.trim().is_empty() {
                e
            } else {
                captured
            }),
        },
    }
}

#[cfg(unix)]
fn capture_stderr<R>(f: impl FnOnce() -> R) -> (R, String) {
    use std::os::unix::io::FromRawFd;

    // SAFETY: dup/pipe/dup2/close on fd 2 with valid arguments. We restore
    // fd 2 at the end. Between dup2 and restore, anything written to stderr
    // by the closure (rustc diagnostics, eprintln!, etc.) lands in the pipe.
    let saved = unsafe { libc::dup(libc::STDERR_FILENO) };
    if saved < 0 {
        return (f(), String::new());
    }

    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        unsafe { libc::close(saved) };
        return (f(), String::new());
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);

    if unsafe { libc::dup2(write_fd, libc::STDERR_FILENO) } < 0 {
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
            libc::close(saved);
        };
        return (f(), String::new());
    }
    unsafe { libc::close(write_fd) };

    // Drain the pipe on a thread so that writers don't block when the pipe
    // fills.
    let reader = std::thread::spawn(move || -> String {
        let mut buf = Vec::new();
        let mut file = unsafe { std::fs::File::from_raw_fd(read_fd) };
        let _ = file.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });

    let result = f();
    let _ = std::io::stderr().flush();

    // Restore stderr. Once dup2 detaches fd 2 from the pipe, the pipe has no
    // remaining writer, so the reader thread will see EOF and return.
    unsafe {
        libc::dup2(saved, libc::STDERR_FILENO);
        libc::close(saved);
    }

    let captured = reader.join().unwrap_or_default();
    (result, captured)
}

#[cfg(not(unix))]
fn capture_stderr<R>(f: impl FnOnce() -> R) -> (R, String) {
    (f(), String::new())
}
