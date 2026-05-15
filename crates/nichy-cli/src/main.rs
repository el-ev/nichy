#![feature(rustc_private)]

extern crate rustc_driver;

mod render;
mod serve;

use std::io::{IsTerminal, Read};
use std::path::Path;
use std::time::Duration;

enum InputSource {
    Inline(String),
    File(String),
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut color = false;
    let mut input_source = None;
    let mut sysroot: Option<String> = None;
    let mut target: Option<String> = None;
    let mut json_output = false;
    let mut show_footer = true;
    let mut verbose = false;
    let mut timeout: Option<f64> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--no-color" => color = false,
            "--color" => color = true,
            "--json" => json_output = true,
            "--no-footer" => show_footer = false,
            "-v" | "--verbose" => verbose = true,
            "--serve" => serve::run(),
            "--timeout" => {
                i += 1;
                if i < args.len() {
                    match args[i].parse::<f64>() {
                        Ok(s) if s > 0.0 && s.is_finite() => timeout = Some(s),
                        _ => {
                            eprintln!("error: --timeout requires a positive number of seconds");
                            std::process::exit(1);
                        }
                    }
                } else {
                    eprintln!("error: --timeout requires a number of seconds");
                    std::process::exit(1);
                }
            }
            "-V" | "--version" => {
                let hash = env!("NICHY_RUSTC_HASH");
                let hash_suffix = if hash.is_empty() {
                    String::new()
                } else {
                    format!(" ({hash})")
                };
                println!(
                    "nichy {} · {}{hash_suffix}",
                    env!("CARGO_PKG_VERSION"),
                    env!("NICHY_RUSTC_VERSION")
                );
                return;
            }
            "--sysroot" => {
                i += 1;
                if i < args.len() {
                    sysroot = Some(args[i].clone());
                } else {
                    eprintln!("error: --sysroot requires a path");
                    std::process::exit(1);
                }
            }
            "--target" => {
                i += 1;
                if i < args.len() {
                    target = Some(args[i].clone());
                } else {
                    eprintln!("error: --target requires a triple");
                    std::process::exit(1);
                }
            }
            "-t" | "--type" => {
                i += 1;
                if i < args.len() {
                    input_source = Some(InputSource::Inline(args[i].clone()));
                } else {
                    eprintln!("error: -t requires a type expression");
                    std::process::exit(1);
                }
            }
            "-h" | "--help" => {
                print_help();
                return;
            }
            arg if !arg.starts_with('-') => {
                input_source = Some(InputSource::File(arg.to_string()));
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    if !has_explicit_color(&args) {
        color = atty_stdout();
    }

    if let Some(secs) = timeout {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(secs));
            eprintln!("error: nichy timed out after {secs}s");
            std::process::exit(124);
        });
    }

    let sysroot_path = sysroot.as_deref().map(Path::new);
    let target_triple = target.as_deref();

    let layouts = match input_source {
        Some(InputSource::Inline(ref expr)) => {
            nichy_rustc::analyze_type_expr(expr, sysroot_path, target_triple)
        }
        Some(InputSource::File(ref path)) => {
            nichy_rustc::analyze_file(Path::new(path), sysroot_path, target_triple)
        }
        None => {
            if atty_stdin() {
                print_help();
                print_examples();
                return;
            }
            let mut buf = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
                eprintln!("error: failed to read stdin: {e}");
                std::process::exit(1);
            }
            nichy_rustc::analyze_snippet(&buf, sysroot_path, target_triple)
        }
    };

    match layouts {
        Ok(layouts) => {
            if layouts.is_empty() {
                std::process::exit(1);
            }
            if json_output {
                println!("{}", serde_json::to_string(&layouts).unwrap());
            } else {
                let ctx = render::Ctx::new(color, verbose);
                for tl in &layouts {
                    print!("{}", render::render_type_layout(tl, &ctx));
                }
                if show_footer {
                    print!(
                        "{}",
                        render::render_footer(
                            env!("NICHY_RUSTC_VERSION"),
                            env!("NICHY_RUSTC_HASH"),
                            &ctx,
                        )
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn has_explicit_color(args: &[String]) -> bool {
    args.iter().any(|a| a == "--color" || a == "--no-color")
}

fn atty_stdout() -> bool {
    std::io::stdout().is_terminal()
}
fn atty_stdin() -> bool {
    std::io::stdin().is_terminal()
}

fn print_help() {
    eprintln!(
        "\
nichy — Rust type layout & niche optimization visualizer
        powered by rustc

USAGE:
    nichy [OPTIONS] [FILE]
    nichy -t \"Option<&u64>\"
    echo 'struct Foo {{ x: u32, y: bool }}' | nichy

OPTIONS:
    -t, --type TYPE         Analyze a single type expression
    --target TRIPLE         Target triple (e.g. aarch64-unknown-linux-gnu)
    --sysroot PATH          Sysroot for rustc (default: $NICHY_SYSROOT)
    --color / --no-color    Force color on/off
    --no-footer             Hide version footer
    --timeout SECS          Abort if analysis exceeds SECS wall-clock seconds
    -V, --version           Print version info
    -h, --help              Show this help

ENVIRONMENT:
    NICHY_SYSROOT    Sysroot path (stage1 compiler)"
    );
}

fn print_examples() {
    eprintln!(
        "\
\nEXAMPLES:
    nichy -t \"Option<&u64>\"
    nichy -t \"Result<u32, bool>\"

    echo '
    struct Padded {{
        a: u8,
        b: u64,
        c: u8,
    }}
    ' | nichy

    echo '
    enum Shape {{
        Circle(f64),
        Rectangle {{ w: f64, h: f64 }},
        Point,
    }}
    ' | nichy

    nichy src/types.rs"
    );
}
