use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use serde::Serialize;

use nichy::{InfraReason, JobKind, WorkerRequest, WorkerResponse};

#[derive(Serialize)]
struct Ready {
    ready: bool,
    pid: u32,
}

pub fn run() -> ! {
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

fn handle_one(line: &str) -> WorkerResponse {
    let req: WorkerRequest<'_> = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return WorkerResponse::UserError {
                message: format!("bad request: {e}"),
            };
        }
    };

    let target = req.target.as_deref();

    let (result, captured) = capture_stderr(|| match req.kind {
        JobKind::Type => nichy_rustc::analyze_type_expr(&req.input, None, target),
        JobKind::Snippet => nichy_rustc::analyze_snippet(&req.input, None, target),
        JobKind::File => nichy_rustc::analyze_file(Path::new(req.input.as_ref()), None, target),
    });

    let fallback = match result {
        Ok(layouts) if !layouts.is_empty() => return WorkerResponse::Ok { layouts },
        Ok(_) => "no types analyzed".to_string(),
        Err(e) => e,
    };
    let message = if captured.trim().is_empty() {
        fallback
    } else {
        captured
    };
    classify(message)
}

// Substrings that fingerprint a poisoned worker rather than bad user
// input. Each maps to the reason tag the pool sees on the wire.
const INFRA_PATTERNS: &[(&str, InfraReason)] = &[
    ("only metadata stub found for", InfraReason::MetadataStub),
    (
        "failed to mmap rmeta metadata",
        InfraReason::RmetaMmapFailed,
    ),
    ("found invalid metadata files", InfraReason::InvalidMetadata),
    ("E0786", InfraReason::InvalidMetadata),
    ("can't find crate for `std`", InfraReason::MissingStd),
    ("can't find crate for `core`", InfraReason::MissingCore),
    ("can't find crate for `alloc`", InfraReason::MissingAlloc),
    ("the compiler unexpectedly panicked", InfraReason::Ice),
    ("internal compiler error:", InfraReason::Ice),
    ("'rustc' panicked at", InfraReason::Ice),
];

fn classify_infra(text: &str) -> Option<InfraReason> {
    INFRA_PATTERNS
        .iter()
        .find(|(pat, _)| text.contains(pat))
        .map(|(_, reason)| *reason)
}

fn classify(message: String) -> WorkerResponse {
    match classify_infra(&message) {
        Some(reason) => WorkerResponse::InfraError { message, reason },
        None => WorkerResponse::UserError { message },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_metadata_stub_is_infra() {
        let s = "error: only metadata stub found for `rlib` dependency `core` please provide path to the corresponding .rmeta file";
        assert_eq!(classify_infra(s), Some(InfraReason::MetadataStub));
    }

    #[test]
    fn classify_missing_std_is_infra() {
        let s = "error[E0463]: can't find crate for `std`";
        assert_eq!(classify_infra(s), Some(InfraReason::MissingStd));
    }

    #[test]
    fn classify_rmeta_mmap_is_infra() {
        let s = "error: failed to mmap rmeta metadata: '/path/libstd.rmeta'";
        assert_eq!(classify_infra(s), Some(InfraReason::RmetaMmapFailed));
    }

    #[test]
    fn classify_e0786_is_infra() {
        let s = "error[E0786]: found invalid metadata files for crate `std`";
        assert_eq!(classify_infra(s), Some(InfraReason::InvalidMetadata));
    }

    #[test]
    fn classify_ice_is_infra() {
        let s = "error: internal compiler error: query stack is empty";
        assert_eq!(classify_infra(s), Some(InfraReason::Ice));
    }

    #[test]
    fn classify_user_syntax_error_is_not_infra() {
        let s = "error: expected one of `,`, `:`, or `}`, found `;`";
        assert_eq!(classify_infra(s), None);
    }

    #[test]
    fn classify_unknown_ident_is_not_infra() {
        let s = "error[E0412]: cannot find type `NotAType` in this scope";
        assert_eq!(classify_infra(s), None);
    }

    #[test]
    fn classify_helper_wraps_into_response() {
        match classify("error: only metadata stub found for blah".into()) {
            WorkerResponse::InfraError { reason, .. } => {
                assert_eq!(reason, InfraReason::MetadataStub);
            }
            _ => panic!("expected InfraError"),
        }
        match classify("error: expected `;`".into()) {
            WorkerResponse::UserError { .. } => {}
            _ => panic!("expected UserError"),
        }
    }
}
