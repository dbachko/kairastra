FROM rust:1.88-bookworm AS builder

WORKDIR /src/rust
COPY rust/Cargo.toml ./Cargo.toml
COPY rust/src ./src
COPY rust/README.md ./README.md

RUN cargo build --release

FROM node:20-bookworm-slim AS runtime

ARG CODEX_CLI_VERSION=0.114.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates git \
    && npm install -g "@openai/codex@${CODEX_CLI_VERSION}" \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/rust/target/release/symphony-rust /usr/local/bin/symphony-rust

WORKDIR /app

ENV RUST_LOG=info
ENV SYMPHONY_WORKSPACE_ROOT=/workspaces

ENTRYPOINT ["symphony-rust"]
