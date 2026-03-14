FROM rust:1.88-bookworm AS builder

WORKDIR /src/rust
COPY rust/Cargo.toml ./Cargo.toml
COPY rust/Cargo.lock ./Cargo.lock
COPY rust/src ./src
COPY rust/README.md ./README.md

RUN cargo build --locked --release

FROM node:20-bookworm-slim AS runtime

ARG CODEX_CLI_VERSION=0.114.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates git \
    && npm install -g "@openai/codex@${CODEX_CLI_VERSION}" \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/rust/target/release/symphony-rust /usr/local/bin/symphony-rust
COPY rust/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

WORKDIR /app

ENV RUST_LOG=info
ENV SYMPHONY_WORKSPACE_ROOT=/workspaces

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
