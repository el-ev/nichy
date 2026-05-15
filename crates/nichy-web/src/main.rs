use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use proc_macro2::{TokenStream, TokenTree};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;

use nichy::TypeLayout;

mod cache;
mod config;
mod db;
mod hash;
mod pool;
mod runner;
mod snippets;
mod stats;

use cache::{AnalysisCache, CacheKey};
use config::SiteConfig;
use snippets::SnippetStore;
use stats::Stats;

const FORBIDDEN_MACROS: &[&str] = &[
    "include",
    "include_str",
    "include_bytes",
    "env",
    "option_env",
    "asm",
    "global_asm",
    "naked_asm",
    "concat_idents",
];

fn reject_forbidden_macros(code: &str) -> Result<(), String> {
    let tokens: TokenStream = code
        .parse()
        .map_err(|e: proc_macro2::LexError| format!("could not tokenize input: {e}"))?;
    scan_tokens(tokens, false).map(|_| ())
}

fn scan_tokens(stream: TokenStream, mut in_use: bool) -> Result<bool, String> {
    let mut iter = stream.into_iter().peekable();
    while let Some(tt) = iter.next() {
        match tt {
            TokenTree::Ident(id) => {
                let s = id.to_string();
                if s == "use" {
                    in_use = true;
                    continue;
                }
                if !FORBIDDEN_MACROS.contains(&s.as_str()) {
                    continue;
                }
                if in_use {
                    return Err(format!(
                        "identifier `{s}` is not allowed in a `use` statement",
                    ));
                }
                let is_bang =
                    matches!(iter.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '!');
                if is_bang {
                    iter.next();
                    if matches!(iter.peek(), Some(TokenTree::Group(_))) {
                        return Err(format!("macro `{s}!` is not allowed"));
                    }
                }
            }
            TokenTree::Punct(p) => {
                if p.as_char() == ';' {
                    in_use = false;
                }
            }
            TokenTree::Group(g) => {
                in_use = scan_tokens(g.stream(), in_use)?;
            }
            TokenTree::Literal(_) => {}
        }
    }
    Ok(in_use)
}

static NICHY_BIN: LazyLock<String> =
    LazyLock::new(|| std::env::var("NICHY_BIN").unwrap_or_else(|_| "nichy".into()));

fn footer_html(cfg: &SiteConfig) -> String {
    let pkg = env!("CARGO_PKG_VERSION");
    let rustc = env!("NICHY_RUSTC_VERSION");
    let hash = env!("NICHY_RUSTC_HASH");

    let nichy_part = format!(
        "<a href=\"https://crates.io/crates/nichy\">nichy</a> {}",
        html_escape(pkg),
    );
    let rustc_part = if hash.is_empty() {
        html_escape(rustc)
    } else {
        let h = html_escape(hash);
        format!(
            "{} (<a href=\"https://github.com/rust-lang/rust/tree/{h}\">{h}</a>)",
            html_escape(rustc),
        )
    };

    let mut parts = vec![format!("{nichy_part} · {rustc_part}")];
    if let Some(author) = &cfg.author {
        let name = html_escape(author);
        let rendered = match &cfg.author_url {
            Some(url) => format!("<a href=\"{}\">{name}</a>", html_escape(url)),
            None => name,
        };
        parts.push(format!("by {rendered}"));
    }
    parts.join(" · ")
}

fn render_page(template: &str, cfg: &SiteConfig) -> Bytes {
    let site_name = html_escape(cfg.site_name.as_deref().unwrap_or("nichy"));
    Bytes::from(
        template
            .replace("{{SITE_NAME}}", &site_name)
            .replace("{{FOOTER}}", &footer_html(cfg)),
    )
}

const DEFAULT_TARGET: &str = "x86_64-unknown-linux-gnu";

const ALLOWED_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "i686-unknown-linux-gnu",
    "wasm32-unknown-unknown",
];

/// Canonicalize an incoming `target` field: missing/empty → default; otherwise
/// must be a member of `ALLOWED_TARGETS`. Returns the canonical static string.
fn normalize_target(t: Option<&str>) -> Result<&'static str, &'static str> {
    let s = t.map(str::trim).unwrap_or("");
    if s.is_empty() {
        return Ok(DEFAULT_TARGET);
    }
    ALLOWED_TARGETS
        .iter()
        .copied()
        .find(|&allowed| allowed == s)
        .ok_or("invalid target")
}

/// Strip trailing horizontal whitespace from each line. Newlines and leading
/// whitespace are preserved.
fn normalize_content(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for line in s.split('\n') {
        if !first {
            out.push('\n');
        }
        out.push_str(line.trim_end_matches([' ', '\t']));
        first = false;
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[derive(Deserialize)]
struct AnalyzeRequest {
    code: Option<String>,
    #[serde(rename = "type")]
    type_expr: Option<String>,
    target: Option<String>,
}

#[derive(Serialize)]
struct AnalyzeResponse {
    types: Vec<TypeLayout>,
    cached: bool,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Deserialize)]
struct ShortenRequest {
    code: Option<String>,
    #[serde(rename = "type")]
    type_expr: Option<String>,
    target: Option<String>,
}

#[derive(Serialize)]
struct ShortenResponse {
    id: String,
}

#[derive(Serialize)]
struct SnippetResponse {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    type_expr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
}

fn nichy_bin() -> &'static str {
    &NICHY_BIN
}

async fn analyze(State(state): State<Arc<AppState>>, Json(req): Json<AnalyzeRequest>) -> Response {
    let is_type_expr = req.type_expr.is_some();

    let target = match normalize_target(req.target.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            state.stats.record(stats::Outcome::BadRequest);
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse { error: e.into() }),
            )
                .into_response();
        }
    };

    let code = req.code.as_deref().map(normalize_content);
    let type_expr = req.type_expr.as_deref().map(normalize_content);

    for s in [&code, &type_expr].into_iter().flatten() {
        if let Err(e) = reject_forbidden_macros(s) {
            state
                .stats
                .record(stats::Outcome::Forbidden { is_type_expr });
            return (StatusCode::FORBIDDEN, Json(ErrorResponse { error: e })).into_response();
        }
    }

    let cache_key = match (code.as_deref(), type_expr.as_deref()) {
        (_, Some(expr)) => Some(CacheKey::new(true, expr, Some(target))),
        (Some(c), _) => Some(CacheKey::new(false, c, Some(target))),
        _ => None,
    };

    if let Some(key) = &cache_key {
        if let Some(cached) = state.cache.get(key) {
            state.stats.record_cache_hit();
            state.stats.record(stats::Outcome::Success {
                types_count: cached.len() as u64,
                target: Some(target),
                is_type_expr,
            });
            return Json(AnalyzeResponse {
                types: (*cached).clone(),
                cached: true,
            })
            .into_response();
        }
        state.stats.record_cache_miss();
    }

    let job = match (code.as_deref(), type_expr.as_deref()) {
        (_, Some(expr)) => runner::Job::TypeExpr(expr),
        (Some(c), _) => runner::Job::Snippet(c),
        _ => {
            state.stats.record(stats::Outcome::BadRequest);
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse {
                    error: "request must include `code` or `type`".into(),
                }),
            )
                .into_response();
        }
    };

    let inner_attr_lines = match job {
        runner::Job::Snippet(c) => nichy::count_inner_attr_lines(c),
        runner::Job::TypeExpr(_) => 0,
    };

    let result = match &state.pool {
        Some(p) => p.submit(job, Some(target), state.timeout_secs).await,
        None => {
            let sem = state
                .run_semaphore
                .as_ref()
                .expect("run_semaphore must be set when pool is absent");
            let _permit = sem.acquire().await.expect("run_semaphore closed");
            runner::run_nichy(nichy_bin(), job, Some(target), state.timeout_secs).await
        }
    };

    let result = result.map_err(|(status, raw)| {
        let msg = if status == StatusCode::UNPROCESSABLE_ENTITY {
            runner::clean_rustc_error(&raw, inner_attr_lines)
        } else {
            raw
        };
        (status, msg)
    });

    match result {
        Ok(types) => {
            state.stats.record(stats::Outcome::Success {
                types_count: types.len() as u64,
                target: Some(target),
                is_type_expr,
            });
            if let Some(key) = cache_key {
                state.cache.put(key, Arc::new(types.clone()));
            }
            Json(AnalyzeResponse {
                types,
                cached: false,
            })
            .into_response()
        }
        Err((status, error)) => {
            let outcome = match status {
                StatusCode::GATEWAY_TIMEOUT => stats::Outcome::Timeout { is_type_expr },
                StatusCode::UNPROCESSABLE_ENTITY => stats::Outcome::AnalysisError { is_type_expr },
                _ => stats::Outcome::InternalError { is_type_expr },
            };
            state.stats.record(outcome);
            (status, Json(ErrorResponse { error })).into_response()
        }
    }
}

async fn shorten(State(state): State<Arc<AppState>>, Json(req): Json<ShortenRequest>) -> Response {
    let (is_type_expr, content) = match (req.type_expr, req.code) {
        (Some(t), _) => (true, t),
        (_, Some(c)) => (false, c),
        _ => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse {
                    error: "request must include `code` or `type`".into(),
                }),
            )
                .into_response();
        }
    };
    let content = normalize_content(&content);
    if content.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse {
                error: "snippet is empty".into(),
            }),
        )
            .into_response();
    }
    if let Err(e) = reject_forbidden_macros(&content) {
        return (StatusCode::FORBIDDEN, Json(ErrorResponse { error: e })).into_response();
    }
    let target = match normalize_target(req.target.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse { error: e.into() }),
            )
                .into_response();
        }
    };
    match state.snippets.put(is_type_expr, &content, target) {
        Some(id) => {
            state.stats.record_shortlink_created();
            Json(ShortenResponse { id }).into_response()
        }
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "failed to store snippet".into(),
            }),
        )
            .into_response(),
    }
}

async fn snippet(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    if !snippets::is_valid_id(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match state.snippets.get(&id) {
        Some(s) => {
            state.stats.record_shortlink_loaded();
            Json(SnippetResponse {
                type_expr: s.is_type_expr.then(|| s.content.clone()),
                code: (!s.is_type_expr).then(|| s.content.clone()),
                target: (!s.target.is_empty()).then_some(s.target),
            })
            .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn shortlink_page(State(state): State<Arc<AppState>>) -> Html<Bytes> {
    Html(state.index_html.clone())
}

async fn stats_json(State(state): State<Arc<AppState>>) -> Json<stats::StatsSnapshot> {
    Json(state.stats.snapshot(
        state.cache.len(),
        state.cache.capacity,
        state.snippets.len(),
    ))
}

async fn stats_page(State(state): State<Arc<AppState>>) -> Html<Bytes> {
    Html(state.stats_html.clone())
}

async fn about_page(State(state): State<Arc<AppState>>) -> Html<Bytes> {
    Html(state.about_html.clone())
}

async fn index(State(state): State<Arc<AppState>>) -> Html<Bytes> {
    Html(state.index_html.clone())
}

struct AppState {
    index_html: Bytes,
    stats_html: Bytes,
    about_html: Bytes,
    timeout_secs: f64,
    stats: Arc<Stats>,
    cache: Arc<AnalysisCache>,
    snippets: Arc<SnippetStore>,
    run_semaphore: Option<Semaphore>,
    pool: Option<Arc<pool::WorkerPool>>,
}

macro_rules! static_asset {
    ($name:ident, $content_type:expr, $file:literal) => {
        async fn $name() -> (
            [(axum::http::header::HeaderName, &'static str); 1],
            &'static str,
        ) {
            (
                [(axum::http::header::CONTENT_TYPE, $content_type)],
                include_str!($file),
            )
        }
    };
}

static_asset!(favicon, "image/svg+xml", "../static/favicon.svg");
static_asset!(common_css, "text/css", "../static/common.css");
static_asset!(common_js, "application/javascript", "../static/common.js");
static_asset!(robots_txt, "text/plain", "../static/robots.txt");

async fn health() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() {
    let cfg = config::load();

    let index_html = render_page(include_str!("../static/index.html"), &cfg);
    let stats_html = render_page(include_str!("../static/stats.html"), &cfg);
    let about_html = render_page(include_str!("../static/about.html"), &cfg);

    let db = db::open(&PathBuf::from(&cfg.db_path))
        .unwrap_or_else(|e| panic!("failed to open database {}: {e}", cfg.db_path));
    eprintln!("nichy-web db: {}", cfg.db_path);

    let stats = Stats::new(db.clone());
    let cache = AnalysisCache::new(db.clone(), cfg.cache_capacity);
    let snippets = Arc::new(SnippetStore::new(db.clone()));

    // When the pool is enabled, concurrency is gated inside the pool itself.
    // run_semaphore exists only for the spawn-per-request fallback path.
    let (pool, run_semaphore) = if cfg.service_workers == 0 {
        eprintln!("worker pool disabled (service_workers=0); using spawn-per-request");
        let cpus = std::thread::available_parallelism()
            .map(NonZeroUsize::get)
            .unwrap_or(2);
        (None, Some(Semaphore::new(cpus)))
    } else {
        let pool = pool::WorkerPool::new(pool::PoolConfig {
            bin: PathBuf::from(nichy_bin()),
            workers: cfg.service_workers,
            max_jobs_per_worker: cfg.max_jobs_per_worker,
        })
        .await;
        (Some(pool), None)
    };

    let state = Arc::new(AppState {
        index_html,
        stats_html,
        about_html,
        timeout_secs: cfg.timeout_secs,
        stats: stats.clone(),
        cache: cache.clone(),
        snippets: snippets.clone(),
        run_semaphore,
        pool,
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/s/{id}", get(shortlink_page))
        .route("/favicon.svg", get(favicon))
        .route("/robots.txt", get(robots_txt))
        .route("/common.css", get(common_css))
        .route("/common.js", get(common_js))
        .route("/api/analyze", post(analyze))
        .route("/api/shorten", post(shorten))
        .route("/api/snippet/{id}", get(snippet))
        .route("/api/health", get(health))
        .route("/api/stats", get(stats_json))
        .route("/stats", get(stats_page))
        .route("/about", get(about_page))
        .layer(RequestBodyLimitLayer::new(1024 * 64))
        .layer(CorsLayer::permissive())
        .with_state(state);

    eprintln!("using nichy binary: {}", nichy_bin());

    let mut set = tokio::task::JoinSet::new();
    for addr in &cfg.listen {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
        eprintln!("nichy-web listening on http://{addr}");
        let app = app.clone();
        set.spawn(async move { axum::serve(listener, app).await.unwrap() });
    }

    while let Some(result) = set.join_next().await {
        result.unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_rejects_bare_include() {
        assert!(reject_forbidden_macros("include!(\"x\")").is_err());
    }

    #[test]
    fn forbidden_rejects_with_whitespace_before_bang() {
        assert!(reject_forbidden_macros("include !(\"x\")").is_err());
        assert!(reject_forbidden_macros("global_asm ! (\"\")").is_err());
    }

    #[test]
    fn forbidden_rejects_each_macro() {
        for name in [
            "include",
            "include_str",
            "include_bytes",
            "env",
            "option_env",
            "asm",
            "global_asm",
            "naked_asm",
            "concat_idents",
        ] {
            let code = format!("{name}!()");
            assert!(
                reject_forbidden_macros(&code).is_err(),
                "expected `{code}` to be rejected",
            );
        }
    }

    #[test]
    fn forbidden_allows_ident_with_same_name() {
        // No `!` after the identifier -> not a macro invocation.
        assert!(reject_forbidden_macros("let include = 1;").is_ok());
        assert!(reject_forbidden_macros("fn env(x: u8) {}").is_ok());
        assert!(reject_forbidden_macros("struct Asm;").is_ok());
    }

    #[test]
    fn forbidden_ignores_line_comments() {
        assert!(reject_forbidden_macros("// include!(\"x\")\n").is_ok());
    }

    #[test]
    fn forbidden_ignores_block_comments() {
        assert!(reject_forbidden_macros("/* include!(\"x\") */").is_ok());
    }

    #[test]
    fn forbidden_ignores_nested_block_comments() {
        assert!(reject_forbidden_macros("/* /* include!(\"x\") */ still in */").is_ok());
    }

    #[test]
    fn forbidden_ignores_inside_string_literal() {
        assert!(reject_forbidden_macros("let s = \"include!(\\\"x\\\")\";").is_ok());
    }

    #[test]
    fn forbidden_allows_safe_code() {
        let code = "
            struct Foo { x: u32 }
            enum Bar { A, B }
            fn add(a: u32, b: u32) -> u32 { a + b }
        ";
        assert!(reject_forbidden_macros(code).is_ok());
    }

    #[test]
    fn forbidden_rejects_through_raw_string() {
        let code = "let _ = r#\"a\"b\"#;\ninclude!(\"x\");";
        assert!(reject_forbidden_macros(code).is_err());
    }

    #[test]
    fn forbidden_rejects_through_char_literal_quote() {
        let code = "let _ = '\"';\ninclude!(\"x\");";
        assert!(reject_forbidden_macros(code).is_err());
    }

    #[test]
    fn forbidden_rejects_through_byte_string() {
        let code = "let _ = b\"a\\\"b\";\nasm!(\"\");";
        assert!(reject_forbidden_macros(code).is_err());
    }

    #[test]
    fn forbidden_rejects_use_as_alias() {
        assert!(reject_forbidden_macros("use std::include as foo;").is_err());
        assert!(reject_forbidden_macros("use core::env as e;").is_err());
    }

    #[test]
    fn forbidden_rejects_in_use_tree() {
        assert!(reject_forbidden_macros("use std::{include};").is_err());
        assert!(
            reject_forbidden_macros("use std::{collections::HashMap, include_str, sync::Arc};")
                .is_err()
        );
    }

    #[test]
    fn forbidden_rejects_inside_macro_rules_body() {
        let code = "macro_rules! sneak { () => { include!(\"/etc/passwd\") } }";
        assert!(reject_forbidden_macros(code).is_err());
    }

    #[test]
    fn forbidden_allows_normal_use_statements() {
        assert!(reject_forbidden_macros("use std::collections::HashMap;").is_ok());
        assert!(reject_forbidden_macros("use std::sync::{Arc, Mutex};").is_ok());
    }

    #[test]
    fn forbidden_allows_method_or_field_named_like_macro() {
        assert!(reject_forbidden_macros("fn main() { let x = S; x.include(1); }").is_ok());
    }

    #[test]
    fn forbidden_rejects_unparseable_input() {
        assert!(reject_forbidden_macros("let x = \"unterminated").is_err());
    }

    #[test]
    fn html_escape_replaces_all_entities() {
        assert_eq!(html_escape("&<>\"'"), "&amp;&lt;&gt;&quot;&#x27;");
    }

    #[test]
    fn html_escape_passes_through_plain_text() {
        assert_eq!(html_escape("hello world"), "hello world");
        assert_eq!(html_escape(""), "");
    }

    #[test]
    fn html_escape_handles_mixed() {
        assert_eq!(
            html_escape("<a href=\"x\">it's & ok</a>"),
            "&lt;a href=&quot;x&quot;&gt;it&#x27;s &amp; ok&lt;/a&gt;",
        );
    }

    #[test]
    fn footer_html_links_nichy_to_crates_io() {
        let cfg = SiteConfig::default();
        let footer = footer_html(&cfg);
        assert!(footer.contains("<a href=\"https://crates.io/crates/nichy\">nichy</a>"));
    }

    #[test]
    fn footer_html_links_rustc_hash_to_github_when_present() {
        let cfg = SiteConfig::default();
        let footer = footer_html(&cfg);
        let hash = env!("NICHY_RUSTC_HASH");
        if !hash.is_empty() {
            assert!(footer.contains(&format!(
                "<a href=\"https://github.com/rust-lang/rust/tree/{hash}\">{hash}</a>"
            )));
        }
    }

    #[test]
    fn footer_html_includes_author_when_set() {
        let cfg = SiteConfig {
            author: Some("alice".into()),
            ..Default::default()
        };
        let footer = footer_html(&cfg);
        assert!(footer.contains("by alice"));
    }

    #[test]
    fn footer_html_omits_author_when_unset() {
        let cfg = SiteConfig::default();
        let footer = footer_html(&cfg);
        assert!(!footer.contains(" by "));
    }

    #[test]
    fn footer_html_links_author_when_author_url_set() {
        let cfg = SiteConfig {
            author: Some("alice".into()),
            author_url: Some("https://example.com/alice".into()),
            ..Default::default()
        };
        let footer = footer_html(&cfg);
        assert!(footer.contains("by <a href=\"https://example.com/alice\">alice</a>"));
    }

    #[test]
    fn footer_html_escapes_author_url() {
        let cfg = SiteConfig {
            author: Some("alice".into()),
            author_url: Some("https://example.com/?q=\"x\"".into()),
            ..Default::default()
        };
        let footer = footer_html(&cfg);
        assert!(footer.contains("href=\"https://example.com/?q=&quot;x&quot;\""));
    }

    #[test]
    fn normalize_target_defaults_when_empty_or_missing() {
        assert_eq!(normalize_target(None), Ok(DEFAULT_TARGET));
        assert_eq!(normalize_target(Some("")), Ok(DEFAULT_TARGET));
        assert_eq!(normalize_target(Some("   ")), Ok(DEFAULT_TARGET));
    }

    #[test]
    fn normalize_target_accepts_each_allowed_triple() {
        for t in ALLOWED_TARGETS {
            assert_eq!(normalize_target(Some(t)), Ok(*t));
        }
    }

    #[test]
    fn normalize_target_rejects_unknown() {
        assert!(normalize_target(Some("x86_64-apple-darwin")).is_err());
        assert!(normalize_target(Some("../etc/passwd")).is_err());
        assert!(normalize_target(Some("x86")).is_err());
    }

    #[test]
    fn normalize_content_strips_trailing_horizontal_whitespace() {
        assert_eq!(normalize_content("a  \nb\t \nc"), "a\nb\nc");
        assert_eq!(normalize_content("a"), "a");
        assert_eq!(normalize_content("a\n"), "a\n");
        assert_eq!(normalize_content(""), "");
    }

    #[test]
    fn normalize_content_preserves_leading_indent_and_blank_lines() {
        assert_eq!(
            normalize_content("    fn x() {  \n        y();  \n    }  "),
            "    fn x() {\n        y();\n    }",
        );
        assert_eq!(normalize_content("a\n\nb"), "a\n\nb");
        assert_eq!(normalize_content("\n  \n  a"), "\n\n  a");
    }
}
