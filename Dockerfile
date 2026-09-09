# Multi-stage image for the `zg` CLI.
#
# The release binary dynamically links `libzvec_c_api.so` (pulled in via
# `zvec-rust-sys`, no rpath set), so the runtime stage installs that library
# into the system library path (`/usr/local/lib` + `ldconfig`). No
# `LD_LIBRARY_PATH` is needed inside the container.
#
# Build:
#   docker build -t zg .
#
# Run (args after the image pass straight through to `zg`):
#   docker run --rm zg --help
#   docker run --rm -v "$PWD:/work" zg query "retry logic" --mode direct
#   docker run --rm -v "$PWD:/work" zg index --mode direct
#
# Notes:
# - `--mode direct` (in-process engine) is the recommended container mode:
#   index state lives next to the workspace (`.zvec-grep/`), so it persists
#   through the volume mount.
# - `zg server run` binds loopback-only (default `127.0.0.1:7999`), which is
#   unreachable through published ports (`-p 7999:7999`) from outside the
#   container. For daemon/MCP use either stdio over `docker run -i`
#   (`zg server --stdio`), commands in the same container network, or
#   `--network host` on Linux. Daemon home (`~/.zvec-grep`) is ephemeral
#   unless mounted (e.g. `-v zg-home:/root/.zvec-grep`).

# syntax=docker/dockerfile:1

ARG RUST_IMAGE=rust:1-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim

FROM ${RUST_IMAGE} AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --bin zg \
    && cp "$(find target/release/build -name libzvec_c_api.so | head -n 1)" /tmp/libzvec_c_api.so \
    && ldd target/release/zg

FROM ${RUNTIME_IMAGE}
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/zg /usr/local/bin/zg
COPY --from=builder /tmp/libzvec_c_api.so /usr/local/lib/libzvec_c_api.so
RUN ldconfig && ldd /usr/local/bin/zg
WORKDIR /work
EXPOSE 7999
ENTRYPOINT ["zg"]
CMD ["--help"]
