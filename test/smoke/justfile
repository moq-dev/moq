#!/usr/bin/env just --justfile
#
# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation
#
# Convenience wrapper around smoke.sh. The clients install themselves from public
# registries on each run; you bring moq-relay + moq on PATH (cargo/brew/apt).

# Run the smoke matrix (default: rust only). Pass flags through, e.g.
#   just smoke --publishers rust,python,js-web --subscribers rust,python,js-jsdelivr
# Browser variants: js-web, js-esbuild, js-jsdelivr (vite/esbuild bundlers, jsDelivr CDN).
default *args:
    ./smoke.sh {{ args }}

# Alias for the default recipe.
smoke *args:
    ./smoke.sh {{ args }}

# Full cross-language matrix with browser cold-start headroom. Rust, Python, Go,
# and the browser publish; everyone subscribes (swift needs the macOS Xcode toolchain).
full:
    ./smoke.sh --publishers rust,python,go,js-web,js-esbuild,js-jsdelivr --subscribers rust,python,go,swift,kotlin,c,c-pkgconfig,c-cmake,gst,js-web,js-esbuild,js-jsdelivr,js-node,js-bun --timeout 30

# Build both Rust implementations from their latest default branches. Drive
# both relays with the Cloudflare client over WebTransport and raw QUIC.
cloudflare:
    ./cloudflare.sh

# Run moxygen's published protocol interop client through the latest moq-dev
# relay over WebTransport and raw QUIC. Linux Docker is required.
moxygen:
    ./moxygen.sh

# Publish/subscribe through every public relay in the moq-interop-runner
# registry (Cloudflare, moxygen, imquic, ...). Only moq-dev's relay is required;
# the rest are reported in a summary table. WebSocket is a separate column for
# the relays named in relays.sh WEBSOCKET_KEYS. Pass flags through, e.g.
#   just relays --only moq-rs-draft-18 --publishers rust,js-web --subscribers rust,js-web,js-bun
relays *args:
    ./relays.sh {{ args }}

# Token interop: install moq auth in each published flavour and cross-verify.
# The Rust `moq auth` comes from a channel (PATH); @moq/auth comes from npm,
# driven under both node and bun. Default: rust only. Pass flags through, e.g.
#   just token --generators rust,js-node --verifiers rust,js-bun --algorithms HS256
token *args:
    ./token.sh {{ args }}

# Full token matrix: every implementation mints and verifies every other's
# tokens. rust-docker pulls the published moqdev/moq image (needs a
# container runtime; set TOKEN_DOCKER=podman to use podman instead of docker).
token-full:
    ./token.sh --generators rust,js-node,js-bun,rust-docker --verifiers rust,js-node,js-bun,rust-docker

# The "nix" channel: get moq-relay + moq from the moq flake itself
# (a public distribution channel, `nix run github:moq-dev/moq#moq`), instead
# of cargo/brew/apt. Referenced ad-hoc with --refresh so the moq version is the
# latest default-branch build, never locked by this repo. The devShell only
# carries client toolchains; moq is built separately here. Override the source
# with MOQ_FLAKE (e.g. a fork or a pinned ref).
MOQ_FLAKE := env_var_or_default("MOQ_FLAKE", "github:moq-dev/moq")
CACHIX_SUB := "https://kixelated.cachix.org"
CACHIX_KEY := "kixelated.cachix.org-1:CmFcV0lyM6KuVM2m9mih0q4SrAa0XyCsiM7GHrz3KKk="

nix-channel *args:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "building moq-relay + moq from {{ MOQ_FLAKE }} (latest)..."
    nix_build() {
        nix build --refresh --no-link --print-out-paths \
            --extra-substituters "{{ CACHIX_SUB }}" \
            --extra-trusted-public-keys "{{ CACHIX_KEY }}" "$1"
    }
    relay=$(nix_build '{{ MOQ_FLAKE }}#moq-relay')
    cli=$(nix_build '{{ MOQ_FLAKE }}#moq')
    echo "relay: $relay"
    echo "cli:   $cli"
    RELAY_BIN="$relay/bin/moq-relay" MOQ_BIN="$cli/bin/moq" ./smoke.sh {{ args }}

# Negative control: no publisher, every subscriber must time out.
negative *args:
    ./smoke.sh --negative {{ args }}

# Assert we pin nothing stale: no committed lock files, and any version we are
# forced to pin (npm playwright) matches what the toolchain provides.
freshness:
    ./freshness.sh

# Lint the harness. shfmt discovers every shell script (incl. the extensionless
# clients/docker/* wrappers) and reads .editorconfig for style; shellcheck lints
# the same set. Each tool is guarded so it skips when not on PATH (nix develop
# provides them all).
check:
    @if command -v shfmt >/dev/null 2>&1; then shfmt --diff $(shfmt -f .); else echo "shfmt: skipped"; fi
    @if command -v shellcheck >/dev/null 2>&1 && command -v shfmt >/dev/null 2>&1; then shellcheck $(shfmt -f .); else echo "shellcheck: skipped"; fi
    @command -v actionlint >/dev/null && actionlint || echo "actionlint: skipped"
    just freshness

# Compile-check and unit-test the local integrity client against the current
# cloudflare/moq-rs Git dependencies. The end-to-end relay check is `just cloudflare`.
cloudflare-check:
    cargo update --manifest-path clients/cloudflare/Cargo.toml
    cargo test --manifest-path clients/cloudflare/Cargo.toml
