# Dockerfile
# Rust dev template for Docker Sandboxes (Claude Code agent)
FROM docker/sandbox-templates:claude-code

USER root
ARG DEBIAN_FRONTEND=noninteractive

# Native deps commonly needed by Rust crates
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    pkg-config \
    curl \
    ca-certificates \
    git \
    openssh-client \
    libssl-dev \
    clang \
    lld \
    cmake \
    python3 \
    protobuf-compiler \
    jq \
    unzip \
    && rm -rf /var/lib/apt/lists/*

# Install Rust (stable) without clobbering PATH from the base image
ENV RUSTUP_HOME=/opt/rust/rustup \
    CARGO_HOME=/opt/rust/cargo
ENV PATH="${CARGO_HOME}/bin:${PATH}"

RUN mkdir -p "$RUSTUP_HOME" "$CARGO_HOME" \
 && curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
 && rustup component add clippy rustfmt \
 && rustup target add wasm32-unknown-unknown

# Optional (useful) cargo tools. Keep this modest; you can add more if you want.
RUN cargo install --locked \
      cargo-nextest \
      cargo-audit \
      cargo-deny \
      cargo-edit \
      cargo-watch \
      ripgrep

# Ensure agent user can write toolchains/caches
RUN chown -R agent:agent /opt/rust

# --- Runtime defaults tuned for sandbox reliability ---
# - Disable sccache wrapper by default (can re-enable per-command)
# - Limit build parallelism by default (low-memory sandboxes)
# - Put build artifacts in /tmp by default
#   (override per-project if you prefer repo-local target/)
ENV RUSTC_WRAPPER="" \
    CARGO_BUILD_JOBS=1 \
    CARGO_TARGET_DIR=/tmp/cargo-target \
    RUST_BACKTRACE=1

USER agent
