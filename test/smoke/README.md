# moq smoke

### [Latest smoke results](https://moq-dev.github.io/moq/relays/)

Nightly publish/subscribe results for the published clients through public MoQ relays. One row per relay, sorted by name ignoring case, with a passing pair count per transport (WebTransport, QUIC, WebSocket). An empty cell means that relay has no endpoint there. Hover a count for the failure, click it for the logs. The page keeps the last 60 runs and updates from `main`.

This suite installs the published moq packages and checks that they exchange media. [`test/interop`](../interop) is its in-tree companion: it builds each client from this checkout instead, to catch interop regressions before anything is published. A missing wheel, a stale Homebrew formula, a broken `.deb`, or an export lost in packaging shows up as a red cell. A cell passes when the subscriber receives a non-empty frame.

## Lanes

| Lane | Run | Coverage |
| --- | --- | --- |
| Packages | `just smoke`, `just full` | Published clients through a local `moq-relay`. `smoke` is Rust only. `full` is the cross-language matrix. |
| Public relays | `just relays` | Rust publisher and subscriber through every relay in the [moq-interop-runner](https://github.com/englishm/moq-interop-runner) registry. CI publishes with Rust and the browser (`js-web`) and subscribes with Rust, `js-web`, and `js-bun`. On those relays `js-web` and `js-bun` run only on WebTransport (`@moq/web-transport` has no raw QUIC mode); a client that does not belong is omitted. WebSocket is its own column for the relays in `WEBSOCKET_KEYS` (`moq-dev-rs` and `stitcher-moq`): each of their `http(s)` endpoints also runs as `ws(s)`, and every other relay keeps that fallback off. A relay that refuses anonymous root broadcasts is dialed on the path `publish_path` names (`stitcher-moq` publishes under `/moq-test`). That run is what the [results page](https://moq-dev.github.io/moq/relays/) shows. Only `--required` relays fail the run; the default is moq-dev's `cdn.moq.dev`, including its WebSocket endpoint. |
| Upstream Docker runner | `just runner-build`, `just runner UPSTREAM OUTPUT` | Every published registry client against this checkout's relay, and the moq-dev clients against published Docker relays. Uses unpinned upstream HEAD and its HTML report. Local-build-only adapters appear as not run. Only a failed pair of a moq-dev client and this checkout's relay, or a failed setup step, fails the run, after the report is written; every other pair is reported only. CI publishes it to [interop-runner/](https://git.moq.dev/interop-runner/). |
| Cloudflare | `just cloudflare` | Relays built from the heads of [cloudflare/moq-rs](https://github.com/cloudflare/moq-rs)'s `draft-18-dev` branch and moq-dev/moq's default branch, driven by a Cloudflare client over WebTransport and raw QUIC. |
| moxygen | `just moxygen` | Meta's [`moxygen`](https://github.com/facebookexperimental/moxygen) interop client through the latest moq-dev relay. Needs Linux Docker. |
| Tokens | `just token`, `just token-full` | `moq auth` and npm [`@moq/auth`](https://www.npmjs.com/package/@moq/auth) mint and verify each other's JWTs, and each verifier rejects a tampered token. |

Hang media, Cloudflare, and moxygen each have their own lane.

CI runs the matrix nightly and on pull requests that touch `test/smoke/`: [`.github/workflows/smoke.yml`](../../.github/workflows/smoke.yml).

## Clients

`moq-relay` and `moq` come from `PATH` or from `RELAY_BIN` and `CLI_BIN`. Every other client installs from its registry on each run.

| Client | Role | Install |
| --- | --- | --- |
| Rust | publish, subscribe | cargo, Homebrew, apt, `nix build github:moq-dev/moq`, or [`moqdev/*`](https://hub.docker.com/u/moqdev) images |
| Python | publish, subscribe | PyPI [`moq-rs`](https://pypi.org/project/moq-rs/) |
| Go | publish, subscribe | [`moq.dev/moq`](https://pkg.go.dev/moq.dev/moq) |
| Browser | publish, subscribe | npm [`@moq/watch`](https://www.npmjs.com/package/@moq/watch) + [`@moq/publish`](https://www.npmjs.com/package/@moq/publish), via vite, esbuild, or [jsDelivr](https://www.jsdelivr.com/) |
| Native JS | subscribe | npm [`@moq/net`](https://www.npmjs.com/package/@moq/net) + [`@moq/hang`](https://www.npmjs.com/package/@moq/hang) under node and bun |
| Swift | subscribe | SPM [`moq-dev/moq-swift`](https://github.com/moq-dev/moq-swift), macOS |
| Kotlin | subscribe | Maven Central [`dev.moq:moq`](https://central.sonatype.com/artifact/dev.moq/moq) |
| C | subscribe | [`libmoq`](https://github.com/moq-dev/moq/releases) release tarball, linked by hand, pkg-config, or CMake |
| GStreamer | subscribe | [`moq-gst`](https://github.com/moq-dev/moq/releases?q=moq-gst) plugin against system GStreamer |

## Run

This directory's own Nix flake carries the client toolchains, separate from moq's (direnv loads `.envrc` here). Put `moq-relay` and `moq` on `PATH` (cargo, Homebrew, or apt) before the lanes below.

```bash
cd test/smoke
nix develop
cargo install moq-relay moq-cli   # or brew / apt
just full
just relays
just cloudflare
just moxygen
just token
just check    # shfmt, shellcheck, actionlint, freshness
```

`just nix-channel` is a run, not an install. It builds `github:moq-dev/moq#moq-relay` and `#moq` with `nix build --refresh --no-link`, then runs `smoke.sh` with `RELAY_BIN` and `CLI_BIN` pointed at those outputs. Nothing is left on `PATH`, so a later `just full` does not see those binaries. The Docker images work the same way: point `RELAY_BIN` and `CLI_BIN` at the wrappers in [`clients/docker`](clients/docker). Those wrappers are not an install.

`just check` lints the harness. Run the lane you changed.

## Versions

Packages resolve to latest on every run: `@moq/*` on the `latest` tag, PyPI `moq-rs`, `moq.dev/moq`, `moqdev/*` images, and GitHub release tarballs. `cloudflare.sh` and `moxygen.sh` build from unpinned Git HEAD. `test/smoke/` commits no package lock files. `flake.lock` pins the dev toolchain.

npm `playwright` is pinned to the flake's `PLAYWRIGHT_VERSION`. `just freshness` fails when that pin drifts or a lock file is committed. The weekly [Smoke toolchain update](../../.github/workflows/smoke-toolchain.yml) workflow opens or refreshes one draft PR containing both updates; failures go through the existing alerts workflow. Dispatch that workflow when a smoke-flake Dependabot PR is due, and use the coupled PR instead of merging a lock-only update. Dependabot auto-merge refuses smoke-flake updates so a lock-only proposal cannot land ahead of its matching pin.

For a local update, run `just update-toolchain` here (Nix and Python are required), then `just freshness` inside the refreshed smoke shell. The command updates `flake.lock`, evaluates its `PLAYWRIGHT_VERSION` without downloading the toolchains, and writes the matching npm pin. Commit both files together. `just check-toolchain` validates freshness and launches the updated browser in the smoke shell. The scheduled updater runs it before proposing a PR; dispatch Smoke on that branch for the full published-package matrix.

A broken package fails only its own cells.
