# Image for Anthropic's OSS Scanner (https://github.com/anthropics/oss-scanner).
#
# The audit runs offline on 2 CPUs, so this build fetches every crate and prebuilds the network-facing Rust crates,
# their tests, and the moq-relay and moq CLI binaries. threat_model.md, next to this file, says what to attack and how.
# The rest of the workspace (capture, encode, FFI and bindings) needs system media libraries and is out of scope.
FROM rust:1-trixie
ENV DEBIAN_FRONTEND=noninteractive
# aws-lc-sys (rustls' crypto provider) builds with clang and cmake.
RUN apt-get update && apt-get install -y --no-install-recommends clang cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*

# scanner contract: the checkout lives inside the image, at /src
COPY . /src
WORKDIR /src

# rust-toolchain.toml pins the toolchain; rustup installs it on first use.
RUN rustup show active-toolchain || rustup toolchain install

ARG CRATES="kio hang moq-auth moq-clock moq-flate moq-hls moq-ice moq-json moq-loc moq-msf moq-mux moq-net moq-pattern moq-quic moq-relay moq-rtc moq-rtmp moq-rtsp moq-sock moq-srt moq-stats moq-tokio moq-cli"

RUN cargo fetch --locked \
    && cargo build --locked $(printf -- '-p %s ' $CRATES) --bins \
    && cargo test --locked --no-run $(printf -- '-p %s ' $CRATES)

ENV CARGO_NET_OFFLINE=true
