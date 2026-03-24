FROM rust:1.88-bookworm AS builder

WORKDIR /src/rust
COPY rust/Cargo.toml ./Cargo.toml
COPY rust/Cargo.lock ./Cargo.lock
COPY rust/src ./src
COPY rust/README.md ./README.md

RUN cargo build --locked --release

FROM node:24-bookworm-slim AS runtime

ARG CODEX_CLI_VERSION=0.114.0
ARG CLAUDE_CODE_VERSION=2.1.81

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates curl dbus dbus-user-session docker-compose docker.io gcc gh git gnome-keyring gosu jq libc6-dev libsecret-1-0 libsecret-tools make openssh-client pkg-config rsync \
    && npm install -g "@openai/codex@${CODEX_CLI_VERSION}" \
    && claude_bucket="https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases" \
    && case "$(dpkg --print-architecture)" in \
        amd64) claude_arch="x64" ;; \
        arm64) claude_arch="arm64" ;; \
        *) echo "Unsupported architecture for Claude Code" >&2; exit 1 ;; \
      esac \
    && if ldd /bin/ls 2>&1 | grep -q musl; then \
         claude_platform="linux-${claude_arch}-musl"; \
       else \
         claude_platform="linux-${claude_arch}"; \
       fi \
    && if [[ -z "${CLAUDE_CODE_VERSION}" || "${CLAUDE_CODE_VERSION}" == "latest" || "${CLAUDE_CODE_VERSION}" == "stable" ]]; then \
         claude_version="$(curl -fsSL "${claude_bucket}/latest")"; \
       else \
         claude_version="${CLAUDE_CODE_VERSION}"; \
       fi \
    && claude_manifest="$(curl -fsSL "${claude_bucket}/${claude_version}/manifest.json")" \
    && claude_checksum="$(jq -r --arg platform "${claude_platform}" '.platforms[$platform].checksum // empty' <<<"${claude_manifest}")" \
    && [[ -n "${claude_checksum}" ]] \
    && curl -fsSL -o /tmp/claude "${claude_bucket}/${claude_version}/${claude_platform}/claude" \
    && echo "${claude_checksum}  /tmp/claude" | sha256sum -c - \
    && install -m 0755 /tmp/claude /usr/local/bin/claude \
    && useradd --create-home --shell /bin/bash symphony \
    && mkdir -p /home/symphony /var/lib/symphony-auth/codex /var/lib/symphony-auth/claude /workspaces \
    && chown -R symphony:symphony /home/symphony /var/lib/symphony-auth /workspaces \
    && rm -f /tmp/claude \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/cargo /usr/local/cargo
COPY --from=builder /usr/local/rustup /usr/local/rustup
COPY --from=builder /usr/local/cargo/bin/ /usr/local/bin/
COPY --from=builder /src/rust/target/release/symphony-rust /usr/local/bin/symphony-rust
COPY rust/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
COPY rust/docker-user-session.sh /usr/local/bin/docker-user-session.sh
COPY rust/docker-compose-shim /usr/local/bin/docker
RUN chmod 0755 /usr/local/bin/docker-entrypoint.sh /usr/local/bin/docker-user-session.sh /usr/local/bin/docker

WORKDIR /app

ENV RUSTUP_HOME=/usr/local/rustup
ENV SYMPHONY_USER=symphony
ENV SYMPHONY_HOME=/home/symphony
ENV PATH=/usr/local/cargo/bin:${PATH}
ENV RUST_LOG=info
ENV SYMPHONY_WORKSPACE_ROOT=/workspaces

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["run"]
