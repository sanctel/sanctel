FROM node:22-bookworm

# Install system dependencies
RUN apt-get update && apt-get install -y \
  git \
  curl \
  jq \
  unzip \
  && rm -rf /var/lib/apt/lists/*

# Install GitHub CLI
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
  | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg \
  && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
  | tee /etc/apt/sources.list.d/github-cli.list > /dev/null \
  && apt-get update && apt-get install -y gh \
  && rm -rf /var/lib/apt/lists/*

# Build-args for UID/GID alignment: sandcastle docker build-image
# defaults these to the host user's UID/GID so image-built files
# and bind-mounted files share an owner without runtime chown.
ARG AGENT_UID=1000
ARG AGENT_GID=1000

# Rename the base image's "node" user to "agent" and align UID/GID.
RUN groupmod -o -g $AGENT_GID node && usermod -o -u $AGENT_UID -g $AGENT_GID -d /home/agent -m -l agent node

# Install Codex CLI.
RUN npm install -g @openai/codex

USER ${AGENT_UID}:${AGENT_GID}

# Install Rust toolchain (Sanctel's Tauri backend; required by `cargo check`,
# `cargo clippy`, `cargo test` in the implement/review/merge feedback loops).
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
  && ~/.cargo/bin/rustup component add clippy rustfmt

# Add cargo to PATH
ENV PATH="/home/agent/.cargo/bin:$PATH"

# Tauri's Linux build deps. Required even for `cargo check`: tauri pulls in
# `-sys` crates (dbus-sys, webkit2gtk-sys, libsoup-sys, atk-sys, ...) whose
# build.rs scripts call pkg-config at check time and abort if the .pc files
# are missing.
#
# tmux is installed in the same root block because the four
# `_against_real_{tmux,zellij}` integration tests in src-tauri/ gate on
# binary availability and silently skip when absent — without tmux + zellij
# in CI, those tests pass-by-skipping rather than verifying anything.
USER root
RUN apt-get update && apt-get install -y \
  build-essential \
  pkg-config \
  libssl-dev \
  libdbus-1-dev \
  libgtk-3-dev \
  libwebkit2gtk-4.1-dev \
  libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev \
  libatk1.0-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  tmux \
  && rm -rf /var/lib/apt/lists/*

# Install zellij pinned to v0.44.3 (matches the maintainer's dev machine,
# so CI and local behavior diverge only when the test catches the drift).
# Prebuilt musl tarball — `cargo install zellij` recompiles from source
# (~5min) and doesn't pin. Arch-detected via dpkg, matching the gh install
# pattern above, so the image builds on both amd64 and arm64 hosts.
# Release URLs (v0.44.3):
#   amd64: https://github.com/zellij-org/zellij/releases/download/v0.44.3/zellij-x86_64-unknown-linux-musl.tar.gz
#   arm64: https://github.com/zellij-org/zellij/releases/download/v0.44.3/zellij-aarch64-unknown-linux-musl.tar.gz
ARG ZELLIJ_VERSION=v0.44.3
RUN set -eux; \
  arch="$(dpkg --print-architecture)"; \
  case "$arch" in \
    amd64) target="x86_64-unknown-linux-musl" ;; \
    arm64) target="aarch64-unknown-linux-musl" ;; \
    *) echo "unsupported arch for zellij install: $arch" >&2; exit 1 ;; \
  esac; \
  curl -fsSL "https://github.com/zellij-org/zellij/releases/download/${ZELLIJ_VERSION}/zellij-${target}.tar.gz" -o /tmp/zellij.tar.gz; \
  tar -xzf /tmp/zellij.tar.gz -C /usr/local/bin zellij; \
  chmod +x /usr/local/bin/zellij; \
  rm /tmp/zellij.tar.gz; \
  zellij --version
USER ${AGENT_UID}:${AGENT_GID}

WORKDIR /home/agent

# In worktree sandbox mode, Sandcastle bind-mounts the git worktree at /home/agent/workspace
# and overrides the working directory to /home/agent/workspace at container start.
# Structure your Dockerfile so that /home/agent/workspace can serve as the project root.
ENTRYPOINT ["sleep", "infinity"]
