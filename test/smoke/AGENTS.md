Cross-language interop smoke for the published MoQ packages.
`test/interop` builds every client from this checkout; this suite installs the public artifacts instead and checks that they talk.

# Required

- Install the published artifact each cell claims to test.
- MoQ package versions stay at latest: `@moq/*` on the `latest` tag, PyPI `moq-rs`, `moq.dev/moq`, `moqdev/*` images, and GitHub release tarballs. `cloudflare.sh` and `moxygen.sh` build from unpinned Git HEAD.
- No package lock files under `test/smoke/` (`go.sum`, `bun.lock`, `Cargo.lock`, `uv.lock`, and the rest of the list in `.gitignore`). `flake.lock` pins the dev toolchain. The moq flake is not a flake input; `just nix-channel` builds `github:moq-dev/moq#moq-relay` and `#moq` with `--refresh`.
- npm `playwright` is the one version pin, and it matches the flake's `PLAYWRIGHT_VERSION`. Bump the toolchain with `nix flake update` and the pin together.
- `moq-relay` and `moq` come from a channel (PATH, `RELAY_BIN`/`MOQ_BIN`, `just nix-channel`, or `clients/docker`). The harness leaves installing them to that channel.
- A red cell is the result. `mark_broken` drops that package's cells and lets the rest of the matrix run. Retries belong on transient registry and release downloads. A client that failed to exchange media gets no retry, no longer timeout, and no skip.
- A published-package bug is fixed at its source, then goes green here on the next release.
- Hang media (`smoke.sh`), Cloudflare, and moxygen stay on separate lanes.
- In `relays.sh`, only `--required` relays fail the run. The default required relay is moq-dev's own.
- Wire new lanes into `.github/workflows/smoke.yml`. It runs on pull requests that touch `test/smoke/`, and nightly.
- New dependencies use the newest stable version, except the pins `freshness.sh` enforces.

# Development

This directory has its own flake, separate from moq's, so its toolchain floats on its own schedule.
direnv loads it here. Otherwise: `nix develop ./test/smoke --command ...`, or `just test smoke <recipe>` from the root.
The shell carries client toolchains. The moq binaries under test come from a channel.

```bash
just check        # shfmt, shellcheck, actionlint, freshness
just smoke        # rust-only matrix; flags pass through to smoke.sh
just full         # cross-language matrix
```

`just check` does not run the matrix. Run the lane you changed (`just smoke`, `just token`, `just cloudflare`, `just moxygen`, `just relays`).
