#![feature(rustc_private)]

extern crate rustc_abi;
extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

mod convert;
mod extract;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use nichy::TypeLayout;

struct NichyCallbacks {
    results: Arc<Mutex<Vec<TypeLayout>>>,
}

impl rustc_driver::Callbacks for NichyCallbacks {
    fn after_analysis<'tcx>(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        tcx: rustc_middle::ty::TyCtxt<'tcx>,
    ) -> rustc_driver::Compilation {
        let layouts = extract::extract_all_layouts(tcx);
        if let Ok(mut r) = self.results.lock() {
            *r = layouts;
        }
        rustc_driver::Compilation::Stop
    }
}

pub fn analyze_file(
    file_path: &Path,
    sysroot: Option<&Path>,
    target: Option<&str>,
) -> Result<Vec<TypeLayout>, String> {
    let results = Arc::new(Mutex::new(Vec::new()));
    let mut callbacks = NichyCallbacks {
        results: Arc::clone(&results),
    };

    let file_str = file_path.to_str().ok_or("invalid file path")?;
    let mut args = vec![
        "nichy-rustc".to_string(),
        file_str.to_string(),
        "--edition=2024".to_string(),
        "--crate-type=lib".to_string(),
        "-Awarnings".to_string(),
    ];

    if let Some(triple) = target {
        args.push("--target".to_string());
        args.push(triple.to_string());
    }

    if let Some(sysroot) = sysroot {
        args.push("--sysroot".to_string());
        args.push(sysroot.display().to_string());
    } else if let Ok(s) = std::env::var("NICHY_SYSROOT") {
        args.push("--sysroot".to_string());
        args.push(s);
    }

    let exit = rustc_driver::catch_with_exit_code(move || {
        rustc_driver::run_compiler(&args, &mut callbacks);
    });

    if exit != std::process::ExitCode::SUCCESS {
        return Err("rustc analysis failed (compilation errors?)".into());
    }

    Arc::try_unwrap(results)
        .map_err(|_| "internal error: Arc still shared")?
        .into_inner()
        .map_err(|e| format!("lock poisoned: {e}"))
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn analyze_snippet(
    code: &str,
    sysroot: Option<&Path>,
    target: Option<&str>,
) -> Result<Vec<TypeLayout>, String> {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nichy_rustc_{}_{id}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let file = dir.join("probe.rs");
    let (user_attrs, user_body) = nichy::split_inner_attrs(code);
    let full = format!("{user_attrs}{}{user_body}\n", nichy::PREAMBLE);
    std::fs::write(&file, &full).map_err(|e| format!("write: {e}"))?;

    let result = analyze_file(&file, sysroot, target);

    let _ = std::fs::remove_dir_all(&dir);
    result
}

pub fn analyze_type_expr(
    expr: &str,
    sysroot: Option<&Path>,
    target: Option<&str>,
) -> Result<Vec<TypeLayout>, String> {
    let sanitized = add_static_lifetime(expr);
    let code = format!("struct _Probe({sanitized});");
    analyze_snippet(&code, sysroot, target)
}

fn add_static_lifetime(ty: &str) -> String {
    let mut out = String::with_capacity(ty.len() + 64);
    let mut chars = ty.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '&' {
            while let Some(&' ') = chars.peek() {
                chars.next();
            }
            if !matches!(chars.peek(), Some(&'\'')) {
                out.push_str("'static ");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::add_static_lifetime;

    #[test]
    fn bare_ref() {
        assert_eq!(add_static_lifetime("&u64"), "&'static u64");
    }

    #[test]
    fn bare_ref_mut() {
        assert_eq!(add_static_lifetime("&mut u64"), "&'static mut u64");
    }

    #[test]
    fn already_static() {
        assert_eq!(add_static_lifetime("&'static u64"), "&'static u64");
    }

    #[test]
    fn already_named_lifetime() {
        assert_eq!(add_static_lifetime("&'a u64"), "&'a u64");
    }

    #[test]
    fn nested_refs() {
        assert_eq!(add_static_lifetime("Option<&u64>"), "Option<&'static u64>",);
    }

    #[test]
    fn multiple_refs() {
        assert_eq!(
            add_static_lifetime("(&u32, &str)"),
            "(&'static u32, &'static str)",
        );
    }

    #[test]
    fn ref_with_space() {
        assert_eq!(add_static_lifetime("& u64"), "&'static u64");
    }

    #[test]
    fn no_refs() {
        assert_eq!(add_static_lifetime("u64"), "u64");
    }

    #[test]
    fn mixed_lifetimes() {
        assert_eq!(
            add_static_lifetime("(&'a u32, &u64)"),
            "(&'a u32, &'static u64)",
        );
    }
}
