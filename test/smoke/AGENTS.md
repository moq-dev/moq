Cross-language interop smoke for the *published* MoQ packages; `test/interop` covers this checkout.

# Required

- Install the published artifact each cell claims to test, at latest, with no lock files (`freshness.sh` enforces this). The one pin is npm `playwright`, matching the flake's `PLAYWRIGHT_VERSION`; bump both together.
- `moq-relay` and `moq` come from a channel (PATH, `RELAY_BIN`/`CLI_BIN`, `just nix-channel`, or `clients/docker`), never from the harness.
- A red cell is the result. Retry only transient registry and release downloads; a client that failed to exchange media gets no retry, longer timeout, or skip. Fix the package at its source.
- Hang media (`smoke.sh`), Cloudflare, and moxygen stay on separate lanes.
- Only `--required` relays (default: moq-dev's) fail `relays.sh`, and only moq-dev's clients against this checkout's relay fail `runner.py`.
- Wire new lanes into `.github/workflows/smoke.yml`; dispatch it on your branch to try a change.

# Development

This directory has its own flake: direnv loads it, or use `nix develop ./test/smoke --command ...` or `just test smoke <recipe>` from the root.

`just check` lints (shfmt, shellcheck, actionlint, freshness) but doesn't run the matrix. Run the lane you changed: `just smoke`, `just full`, `just token`, `just cloudflare`, `just moxygen`, or `just relays`.
