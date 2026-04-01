FROM rust:1.88-bookworm AS builder

WORKDIR /src/rust
COPY rust/Cargo.toml ./Cargo.toml
COPY rust/Cargo.lock ./Cargo.lock
COPY rust/src ./src
COPY rust/README.md ./README.md

RUN cargo build --locked --release

FROM node:24-bookworm-slim

ARG CODEX_CLI_VERSION=0.114.0
ARG CLAUDE_CODE_VERSION=2.1.81
ARG GEMINI_CLI_VERSION=0.35.0

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates curl gcc gh git jq libc6-dev libsecret-1-0 libsecret-tools openssh-client pkg-config ripgrep rsync \
    && corepack enable pnpm \
    && npm install -g "@openai/codex@${CODEX_CLI_VERSION}" \
    && npm install -g "@google/gemini-cli@${GEMINI_CLI_VERSION}" \
    && claude_bucket="https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases" \
    && case "$(dpkg --print-architecture)" in \
        amd64) claude_arch="x64" ;; \
        arm64) claude_arch="arm64" ;; \
        *) echo "Unsupported architecture for Claude Code" >&2; exit 1 ;; \
      esac \
    && if [[ -z "${CLAUDE_CODE_VERSION}" || "${CLAUDE_CODE_VERSION}" == "latest" || "${CLAUDE_CODE_VERSION}" == "stable" ]]; then \
         claude_version="$(curl -fsSL "${claude_bucket}/latest")"; \
       else \
         claude_version="${CLAUDE_CODE_VERSION}"; \
       fi \
    && claude_manifest="$(curl -fsSL "${claude_bucket}/${claude_version}/manifest.json")" \
    && claude_checksum="$(jq -r --arg platform "linux-${claude_arch}" '.platforms[$platform].checksum // empty' <<<"${claude_manifest}")" \
    && [[ -n "${claude_checksum}" ]] \
    && curl -fsSL -o /tmp/claude "${claude_bucket}/${claude_version}/linux-${claude_arch}/claude" \
    && echo "${claude_checksum}  /tmp/claude" | sha256sum -c - \
    && install -m 0755 /tmp/claude /usr/local/bin/claude \
    && useradd --create-home --shell /bin/bash kairastra \
    && mkdir -p /workspace /var/lib/kairastra-auth \
    && chown -R kairastra:kairastra /workspace /var/lib/kairastra-auth \
    && rm -f /tmp/claude \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/rust/target/release/kairastra /usr/local/bin/kairastra

USER kairastra
WORKDIR /workspace

ENV HOME=/home/kairastra
ENV KAIRASTRA_WORKSPACE_ROOT=/workspace/.kairastra-workspaces
ENV RUST_LOG=info

ENTRYPOINT ["kairastra"]
CMD ["--help"]
