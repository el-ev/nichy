# nichy

Rust type memory layout and niche optimization visualizer, powered by `rustc` internals.

nichy links directly against `rustc` via `#![feature(rustc_private)]` to query `layout_of` for every struct, enum, union, and type alias in your code. It reports exact sizes, alignments, field offsets, padding bytes, discriminant encoding, and niche optimization details. You get the same information the compiler uses, presented in a readable form.

Live instance: [niche.rs](https://niche.rs)

## Usage

### CLI

```
# analyze a type expression
nichy -t 'Option<&u64>'

# analyze a file
nichy src/types.rs

# pipe a snippet
echo 'struct Foo { a: u8, b: u64, c: u8 }' | nichy
```

JSON output for tooling:

```
nichy -t 'Result<u32, bool>' --json
```

Cross-target analysis:

```
nichy -t 'usize' --target aarch64-unknown-linux-gnu
```

### Web

The web service serves a browser UI and a JSON API:

| route | purpose |
| --- | --- |
| `POST /api/analyze` | Analyze `{code, target}` or `{type, target}`; returns layouts |
| `POST /api/shorten` | Persist a snippet, returns a short id |
| `GET /api/snippet/{id}` | Fetch a previously shortened snippet |
| `GET /s/{id}` | Browser-loadable shortlink for a snippet |
| `GET /api/stats` / `/stats` | Aggregate request stats (JSON / page) |
| `GET /about` | Background reading on niche layouts |

```
cargo run -p nichy-web    # serves on 127.0.0.1:3873
```

Configuration is in `nichy-web.toml`:

```toml
site_name = "niche.rs"
listen = ["0.0.0.0:3873"]
timeout_secs = 2.0
db_path = "nichy-web.db"
```

## Building

nichy requires a locally-built stage 2 `rustc`. 

It is not very recommended to build `nichy` locally because building `rustc` is slow and takes a lot of disk space.
Use the online instance at [niche.rs](https://niche.rs) if you want to analyze short code snippets.

### 1. Bootstrap rustc

```sh
git submodule update --init rust
cd rust
python3 x.py build library --stage 2 --target x86_64-unknown-linux-gnu
# build library for other targets you want to analyze
# python3 x.py build library --stage 2 \
#   --target aarch64-unknown-linux-gnu,i686-unknown-linux-gnu,wasm32-unknown-unknown
cd ..
```

### 2. Build nichy

```sh
cargo build -p nichy-cli              # CLI
cargo build -p nichy-web              # web service
cargo build --release -p nichy-cli -p nichy-web   # release
```

The workspace `.cargo/config.toml` and `bin/rustc` wrapper handle sysroot and library paths automatically.

### Docker

```sh
docker build -f Dockerfile.rust -t nichy-rust:main .   # once, or on rustc bump
docker build -t nichy-web .
docker run -p 3873:3873 nichy-web
```

## License

[MIT](LICENSE)
