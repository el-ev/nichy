use std::time::Duration;

use axum::http::StatusCode;

use nichy::TypeLayout;

// Resource limits applied to the spawned nichy/rustc subprocess via pre_exec.
// These are a backstop against a snippet that escapes the lexical scanner —
// they don't isolate the filesystem on their own, but combined with the
// non-root user in the Dockerfile they bound the blast radius of any rustc
// path that reads files (include!, env!, etc.) or runs away on memory/CPU.
//
// Tuning notes:
// - AS (virtual memory): the request body is capped at 64 KiB, so even
//   pathological monomorphization stays well under 2 GiB.
// - CPU: per-process cumulative, backstop for a hung rustc the wall-clock
//   timeout fails to kill. Only applied to one-shot subprocesses; long-lived
//   pool workers rely on the per-job tokio timeout instead, since cumulative
//   CPU would otherwise SIGXCPU the worker after a handful of jobs.
// - FSIZE: rustc shouldn't write large files during analysis-only runs.
// - CORE: prevent core dumps from leaking process memory to disk.
#[cfg(unix)]
const RLIMITS_ONESHOT: &[(libc::__rlimit_resource_t, libc::rlim_t)] = &[
    (libc::RLIMIT_AS, 2 * 1024 * 1024 * 1024),
    (libc::RLIMIT_CPU, 30),
    (libc::RLIMIT_FSIZE, 64 * 1024 * 1024),
    (libc::RLIMIT_CORE, 0),
];

#[cfg(unix)]
const RLIMITS_PERSISTENT: &[(libc::__rlimit_resource_t, libc::rlim_t)] = &[
    (libc::RLIMIT_AS, 2 * 1024 * 1024 * 1024),
    (libc::RLIMIT_FSIZE, 64 * 1024 * 1024),
    (libc::RLIMIT_CORE, 0),
];

#[cfg(unix)]
fn apply_rlimits(set: &[(libc::__rlimit_resource_t, libc::rlim_t)]) -> std::io::Result<()> {
    for &(res, val) in set {
        let rlim = libc::rlimit {
            rlim_cur: val,
            rlim_max: val,
        };
        // SAFETY: setrlimit with a valid resource id and rlimit pointer is
        // async-signal-safe per POSIX and the Linux signal(7) man page.
        if unsafe { libc::setrlimit(res, &rlim) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn apply_subprocess_sandbox() -> std::io::Result<()> {
    apply_rlimits(RLIMITS_ONESHOT)
}

#[cfg(unix)]
pub(crate) fn apply_persistent_sandbox() -> std::io::Result<()> {
    apply_rlimits(RLIMITS_PERSISTENT)
}

#[derive(Copy, Clone)]
pub enum Job<'a> {
    TypeExpr(&'a str),
    Snippet(&'a str),
}

pub async fn run_nichy(
    bin: &str,
    job: Job<'_>,
    target: Option<&str>,
    timeout_secs: f64,
) -> Result<Vec<TypeLayout>, (StatusCode, String)> {
    let timeout_str = format!("{timeout_secs}");
    let mut args = vec!["--json", "--no-color", "--timeout", &timeout_str];
    let stdin_data = match job {
        Job::TypeExpr(expr) => {
            args.push("-t");
            args.push(expr);
            None
        }
        Job::Snippet(code) => Some(code),
    };
    if let Some(triple) = target {
        args.push("--target");
        args.push(triple);
    }

    let needs_stdin = stdin_data.is_some();

    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(&args)
        .stdin(if needs_stdin {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    #[cfg(unix)]
    // SAFETY: pre_exec runs in the forked child between fork() and execve();
    // only async-signal-safe operations are permitted. setrlimit and a
    // sigaction reset are both on the safe list.
    unsafe {
        cmd.pre_exec(apply_subprocess_sandbox);
    }

    let mut child = cmd.spawn().map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("failed to spawn nichy ({bin}): {e}"),
        )
    })?;

    if let Some(data) = stdin_data {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().expect("stdin piped");
        stdin
            .write_all(data.as_bytes())
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("write stdin: {e}")))?;
    }

    let wall_clock = Duration::from_secs_f64(timeout_secs + 1.0);
    let output = tokio::time::timeout(wall_clock, child.wait_with_output())
        .await
        .map_err(|_| {
            (
                StatusCode::GATEWAY_TIMEOUT,
                format!("analysis exceeded {timeout_secs}s timeout"),
            )
        })?
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("wait: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err((StatusCode::UNPROCESSABLE_ENTITY, stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<Vec<TypeLayout>>(&stdout)
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("parse nichy output: {e}")))
}

enum Snippet {
    Plain(String),
    Arrow(String),
    Source { num: String, rest: String },
    Gutter { rest: String },
}

pub fn clean_rustc_error(raw: &str, inner_attr_lines: usize) -> String {
    let preamble_lines = nichy::PREAMBLE_LINES;

    let mut snippets: Vec<Snippet> = Vec::new();
    let mut max_num_width = 0usize;

    for line in raw.lines() {
        if line.starts_with("error: aborting due to")
            || line.starts_with("For more information about")
            || line.starts_with("error: rustc analysis failed")
        {
            continue;
        }

        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("--> ") {
            let adjusted = adjust_location(rest, preamble_lines, inner_attr_lines);
            snippets.push(Snippet::Arrow(adjusted));
            continue;
        }

        if let Some((num_str, rest)) = parse_source_line(line)
            && let Ok(num) = num_str.parse::<usize>() {
                let adj = adjust_line_num(num, preamble_lines, inner_attr_lines);
                let adj_str = adj.to_string();
                max_num_width = max_num_width.max(adj_str.len());
                snippets.push(Snippet::Source {
                    num: adj_str,
                    rest: rest.to_string(),
                });
                continue;
            }

        if let Some(rest) = parse_gutter_line(line) {
            snippets.push(Snippet::Gutter {
                rest: rest.to_string(),
            });
            continue;
        }

        snippets.push(Snippet::Plain(line.to_string()));
    }

    let indent = " ".repeat(max_num_width);
    let mut out = String::new();
    let mut prev_blank = true;
    for snip in &snippets {
        let rendered = match snip {
            Snippet::Plain(s) => s.clone(),
            Snippet::Arrow(text) => format!("{indent}--> {text}"),
            Snippet::Source { num, rest } => {
                format!("{num:>max_num_width$} |{rest}")
            }
            Snippet::Gutter { rest } => {
                format!("{indent} |{rest}")
            }
        };
        let is_blank = rendered.trim().is_empty();
        if is_blank && prev_blank {
            continue;
        }
        out.push_str(&rendered);
        out.push('\n');
        prev_blank = is_blank;
    }
    out.trim().to_string()
}

fn parse_source_line(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    let digit_end = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digit_end == 0 {
        return None;
    }
    let after_digits = &trimmed[digit_end..];
    let space_end = after_digits.bytes().take_while(|b| *b == b' ').count();
    if space_end == 0 {
        return None;
    }
    let rest = after_digits[space_end..].strip_prefix('|')?;
    Some((&trimmed[..digit_end], rest))
}

fn parse_gutter_line(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix('|')
}

fn adjust_line_num(line: usize, preamble_lines: usize, inner_attr_lines: usize) -> usize {
    if line <= inner_attr_lines {
        line
    } else {
        line.saturating_sub(preamble_lines)
    }
}

fn adjust_location(loc: &str, preamble_lines: usize, inner_attr_lines: usize) -> String {
    if let Some(colon_pos) = loc.find(".rs:") {
        let after = &loc[colon_pos + 4..];
        if let Some((line_str, rest)) = after.split_once(':')
            && let Ok(line) = line_str.parse::<usize>()
        {
            let adjusted = adjust_line_num(line, preamble_lines, inner_attr_lines);
            return format!("line {adjusted}:{rest}");
        }
    }
    loc.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjust_subtracts_preamble_from_user_line() {
        // Preamble is 10 lines; an error on line 15 of the wrapped file
        // is line 5 of the user's input.
        assert_eq!(
            adjust_location("probe.rs:15:5: error", 10, 0),
            "line 5:5: error",
        );
    }

    #[test]
    fn adjust_keeps_inner_attr_lines_unchanged() {
        // Inner attrs stay above the preamble in the wrapper, so their
        // line numbers match the user's input directly.
        assert_eq!(
            adjust_location("probe.rs:2:1: error", 10, 3),
            "line 2:1: error",
        );
    }

    #[test]
    fn adjust_saturates_when_line_below_preamble() {
        // Shouldn't underflow on degenerate input.
        assert_eq!(
            adjust_location("probe.rs:3:1: error", 10, 0),
            "line 0:1: error",
        );
    }

    #[test]
    fn adjust_returns_input_when_no_rs_suffix() {
        assert_eq!(adjust_location("not a location", 10, 0), "not a location",);
    }

    #[test]
    fn adjust_returns_input_when_line_unparseable() {
        assert_eq!(
            adjust_location("probe.rs:abc:1: error", 10, 0),
            "probe.rs:abc:1: error",
        );
    }

    #[test]
    fn clean_drops_aborting_and_for_more_info() {
        let raw = "error[E0001]: real problem\n  --> probe.rs:15:5\nerror: aborting due to 1 previous error\nFor more information about this error, try `rustc --explain E0001`.\n";
        let cleaned = clean_rustc_error(raw, 0);
        assert!(cleaned.contains("real problem"));
        assert!(!cleaned.contains("aborting"));
        assert!(!cleaned.contains("For more information"));
    }

    #[test]
    fn clean_rewrites_location_pointer() {
        let preamble = nichy::PREAMBLE.lines().count();
        let raw = format!("error[E0001]: oops\n  --> probe.rs:{}:5\n", preamble + 3);
        let cleaned = clean_rustc_error(&raw, 0);
        assert!(cleaned.contains("--> line 3:5"), "got: {cleaned}");
    }

    #[test]
    fn clean_rewrites_source_context_line_numbers() {
        let preamble = nichy::PREAMBLE.lines().count();
        let raw = format!(
            "error: oops\n  --> probe.rs:{user_line}:1\n   |\n{wrapped_a} | z\n   |  - unexpected\n{wrapped_b} | type Ref;\n   | ^^^^ unexpected\n",
            user_line = preamble + 7,
            wrapped_a = preamble + 6,
            wrapped_b = preamble + 7,
        );
        let cleaned = clean_rustc_error(&raw, 0);
        assert!(cleaned.contains("6 | z"), "got: {cleaned}");
        assert!(cleaned.contains("7 | type Ref;"), "got: {cleaned}");
        assert!(
            !cleaned.contains(&format!("{} | z", preamble + 6)),
            "wrapped line number leaked through: {cleaned}",
        );
    }

    #[test]
    fn clean_drops_rustc_analysis_failed_marker() {
        let raw =
            "error: rustc analysis failed (compilation errors?)\nerror[E0001]: real problem\n";
        let cleaned = clean_rustc_error(raw, 0);
        assert!(cleaned.contains("real problem"));
        assert!(!cleaned.contains("rustc analysis failed"));
    }
}
