# Dockerfile
FROM docker/sandbox-templates:claude-code

USER root
ARG DEBIAN_FRONTEND=noninteractive

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
    # Nice-to-have: alternative linker (often reduces memory/time)
    mold \
    && rm -rf /var/lib/apt/lists/*

# IMPORTANT: don't clobber base PATH (keeps `claude` discoverable)
ENV PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:${PATH}"

# Put rustup/cargo outside the bind-mounted workspace and make them agent-writable
# (also avoids polluting /home if that’s on a flaky backing store)
ENV RUSTUP_HOME=/opt/rust/rustup \
    CARGO_HOME=/opt/rust/cargo
ENV PATH="${CARGO_HOME}/bin:${PATH}"

RUN mkdir -p "$RUSTUP_HOME" "$CARGO_HOME" \
 && curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
 && rustup component add clippy rustfmt \
 && rustup target add wasm32-unknown-unknown

# Useful cargo tools (keep minimal; installs are write-heavy)
RUN cargo install --locked \
      cargo-nextest \
      cargo-edit \
      cargo-watch \
      ripgrep

# Default build/cache locations: avoid bind-mounted repo entirely.
# We use /var/tmp rather than /tmp to reduce chance of tmpfs quirks;
# and we control rustc temp explicitly.
ENV CARGO_TARGET_DIR=/var/tmp/cargo-target \
    CARGO_BUILD_JOBS=1 \
    CARGO_INCREMENTAL=0 \
    RUSTC_WRAPPER="" \
    RUST_BACKTRACE=1 \
    TMPDIR=/var/tmp \
    CARGO_NET_GIT_FETCH_WITH_CLI=true

# Make sure dirs exist and are writable by agent
RUN mkdir -p /var/tmp/cargo-target /var/tmp/rustc-tmp \
 && chown -R agent:agent /opt/rust /var/tmp/cargo-target /var/tmp/rustc-tmp /var/tmp

# Optional: prefer mold when linking (can comment out if you don't want it)
# This helps *when* linking is the bottleneck; it won't fix a broken FS,
# but it reduces memory/time pressure in healthy environments.
ENV RUSTFLAGS="-C link-arg=-fuse-ld=mold"

USER agent
