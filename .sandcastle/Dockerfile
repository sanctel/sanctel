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
USER ${AGENT_UID}:${AGENT_GID}

# Install Claude Code CLI
RUN curl -fsSL https://claude.ai/install.sh | bash

# Install Rust toolchain (Sanctel's Tauri backend; required by `cargo check`,
# `cargo clippy`, `cargo test` in the implement/review/merge feedback loops).
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
  && ~/.cargo/bin/rustup component add clippy rustfmt

# Add Claude and cargo to PATH
ENV PATH="/home/agent/.local/bin:/home/agent/.cargo/bin:$PATH"

# Tauri's Linux build deps (only needed if you actually build/run the desktop
# app in this container; the implement/review/merge agents mostly do
# `cargo check`/`cargo clippy`/`cargo test`, which don't need these. Uncomment
# the block below if you need full Tauri builds in the sandbox.
#
# USER root
# RUN apt-get update && apt-get install -y \
#   libwebkit2gtk-4.1-dev \
#   build-essential \
#   libssl-dev \
#   libgtk-3-dev \
#   libayatana-appindicator3-dev \
#   librsvg2-dev \
#   && rm -rf /var/lib/apt/lists/*
# USER ${AGENT_UID}:${AGENT_GID}

WORKDIR /home/agent

# In worktree sandbox mode, Sandcastle bind-mounts the git worktree at /home/agent/workspace
# and overrides the working directory to /home/agent/workspace at container start.
# Structure your Dockerfile so that /home/agent/workspace can serve as the project root.
ENTRYPOINT ["sleep", "infinity"]
