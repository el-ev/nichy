# nichy-web Docker image
#
# Consumes the prebuilt nichy-rust:main toolchain image (see Dockerfile.rust),
# compiles nichy, and packages nichy-web with std libraries for multiple targets.
#
# Prerequisite (one time, or to refresh rustc):
#   docker build -f Dockerfile.rust -t nichy-rust:main .
#
# Build:
#   docker build -t nichy-web .
#
# Run:
#   docker run -p 3873:3873 nichy-web

ARG RUST_IMAGE=nichy-rust:main

FROM ${RUST_IMAGE} AS nichy-builder

ENV STAGE2=/rust/build/x86_64-unknown-linux-gnu/stage2

COPY . /nichy
WORKDIR /nichy

RUN ln -s /rust /nichy/rust

ENV RUST_ROOT=/rust
ENV RUSTC=/nichy/docker-rustc

RUN printf '#!/bin/sh\nexport LD_LIBRARY_PATH="%s/lib:${LD_LIBRARY_PATH:-}"\nexec "%s/bin/rustc" "$@"\n' \
    "$STAGE2" "$STAGE2" > /nichy/docker-rustc && chmod +x /nichy/docker-rustc

RUN mkdir -p /nichy/.cargo && printf '[target.x86_64-unknown-linux-gnu]\nrustflags = [\n    "-C", "link-arg=-Wl,-rpath,%s/lib",\n    "-C", "link-arg=-Wl,-rpath,%s/lib/rustlib/x86_64-unknown-linux-gnu/lib",\n]\n' \
    "$STAGE2" "$STAGE2" > /nichy/.cargo/config.toml

ENV LD_LIBRARY_PATH="/rust/build/x86_64-unknown-linux-gnu/stage2/lib"
ENV PATH="/rust/build/x86_64-unknown-linux-gnu/stage0/bin:${PATH}"

RUN cargo build --release -p nichy-cli -p nichy-web

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --system --no-create-home --uid 65532 nichy

WORKDIR /app

COPY --from=nichy-builder /rust/build/x86_64-unknown-linux-gnu/stage2 /sysroot
COPY --from=nichy-builder /nichy/target/release/nichy /app/nichy
COPY --from=nichy-builder /nichy/target/release/nichy-web /app/nichy-web

RUN mkdir -p /var/lib/nichy && chown nichy:nichy /var/lib/nichy

ENV NICHY_SYSROOT=/sysroot
ENV NICHY_BIN=/app/nichy
ENV LD_LIBRARY_PATH="/sysroot/lib:/sysroot/lib/rustlib/x86_64-unknown-linux-gnu/lib"

RUN printf 'listen = ["0.0.0.0:3873"]\ndb_path = "/var/lib/nichy/nichy-web.db"\n' \
    > /app/nichy-web.toml

USER nichy

EXPOSE 3873

CMD ["/app/nichy-web"]
