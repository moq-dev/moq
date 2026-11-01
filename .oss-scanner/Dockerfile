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
# CI runs tests with nextest, one process per test, and some tests rely on that isolation.
RUN rustup show active-toolchain || rustup toolchain install \
    && case "$(uname -m)" in aarch64) nextest=linux-arm ;; *) nextest=linux ;; esac \
    && curl -LsSf "https://get.nexte.st/latest/$nextest" | tar zxf - -C "$CARGO_HOME/bin" \
    && cargo nextest --version

ARG CRATES="kio hang moq-auth moq-e2ee moq-hls moq-ice moq-json moq-loc moq-msf moq-mux moq-net moq-pattern moq-quic moq-relay moq-rtc moq-rtmp moq-rtsp moq-sock moq-srt moq-stats moq-tokio moq-uring moq-cli"

# Path specs, because the lockfile also has published copies of some workspace crates.
# io-uring compiles the relay's io_uring QUIC workers (`--runtime-io-uring`), which the default features leave out.
RUN cargo fetch --locked \
    && specs=$(printf -- '-p path+file:///src/rs/%s ' $CRATES) \
    && cargo build --locked $specs --features moq-relay/io-uring --bins \
    && cargo test --locked --no-run $specs --features moq-relay/io-uring

ENV CARGO_NET_OFFLINE=true
