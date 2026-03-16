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
    && apt-get install -y --no-install-recommends bash ca-certificates docker-compose docker.io gcc gh git libc6-dev make openssh-client pkg-config rsync \
    && npm install -g "@openai/codex@${CODEX_CLI_VERSION}" \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/cargo /usr/local/cargo
COPY --from=builder /usr/local/rustup /usr/local/rustup
COPY --from=builder /usr/local/cargo/bin/ /usr/local/bin/
COPY --from=builder /src/rust/target/release/symphony-rust /usr/local/bin/symphony-rust
COPY rust/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
COPY rust/docker-compose-shim /usr/local/bin/docker

WORKDIR /app

ENV RUSTUP_HOME=/usr/local/rustup
ENV PATH=/usr/local/cargo/bin:${PATH}
ENV RUST_LOG=info
ENV SYMPHONY_WORKSPACE_ROOT=/workspaces

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["run"]
