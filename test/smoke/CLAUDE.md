Cross-language interop smoke for the published MoQ packages.
The implementations live in [moq-dev/moq](https://github.com/moq-dev/moq). This repo installs the public artifacts and checks that they talk.

# Context

- Read `PROMPTING.md` before instructing other agents via context, memory, plans, or reviews.
- `README.md` describes the lanes. Keep this file to rules.

# Required

- Pull the latest origin changes before working.
- Dig into the root cause and fix it at the source. Never work around a fixable bug with a retry, sleep, or timeout.
- Fail loud and early. Error on unsupported or malformed input rather than warn and continue: supported or refused.
- Reproduce bugs before fixing them. Land each fix with a regression test that fails without it, when one is easy.
- Keep the PR focused. No unrelated refactors, formatting churn, or drive-by changes; split when in doubt.
- Refactor aggressively for long-term maintainability, but re-evaluate the direction as you learn.
- Propose a course change, even suggest abandoning a PR, rather than finish a half-solution.
- When a decision is the maintainer's (scope, naming, which lane), ask with 2-3 options and a recommendation.
- Wire new lanes into `.github/workflows/smoke.yml`. CI runs the matrix on pull requests and nightly.
- Never edit a `CLAUDE.md`, `CONTRIBUTING.md`, or skill without being prompted, and read `PROMPTING.md` first.
- No em dashes.
- Match the existing conventions, patterns, and naming when possible.
- Fix any outdated docs and comments inline; don't add a separate PR for it.
- Add the AI marker `(Written by <model>)` to any posts on GitHub, excluding commit messages that contain `Co-Authored-By:` trailers.
- Any AI comments may be challenged, and not confused with human maintainers.
- Prompt the user to decide when unsure, but always provide recommendations.
- Don't open a PR, issue, or review comment outside the `moq-dev` and `kixelated` orgs without approval.

# What this repo tests

- Install the published artifact each cell claims to test.
- MoQ package versions stay at latest: `@moq/*` on the `latest` tag, PyPI `moq-rs`, `moq.dev/moq`, `moqdev/*` images, and GitHub release tarballs. `cloudflare.sh` and `moxygen.sh` build from unpinned Git HEAD.
- No package lock files (`go.sum`, `bun.lock`, `Cargo.lock`, `uv.lock`, and the rest of the list in `.gitignore`). `flake.lock` pins the dev toolchain. The moq flake is not a flake input; `just nix-channel` builds `github:moq-dev/moq#moq-relay` and `#moq` with `--refresh`.
- npm `playwright` is the one version pin, and it matches the flake's `PLAYWRIGHT_VERSION`. Bump the toolchain with `nix flake update` and the pin together.
- `moq-relay` and `moq` come from a channel (PATH, `RELAY_BIN`/`MOQ_BIN`, `just nix-channel`, or `clients/docker`). The harness leaves installing them to that channel.
- A red cell is the result. `mark_broken` drops that package's cells and lets the rest of the matrix run. Retries belong on transient registry and release downloads. A client that failed to exchange media gets no retry, no longer timeout, and no skip.
- A published-package bug is fixed in its own repo, then goes green here on the next release.
- Hang media (`smoke.sh`), Cloudflare, and moxygen stay on separate lanes.
- In `relays.sh`, only `--required` relays fail the run. The default required relay is moq-dev's own.

# Guidelines

- Prefer a maintained tool over hand-rolling non-core functionality.
- New dependencies use the newest stable version, except the pins `freshness.sh` enforces.
- Comments explain the non-obvious why, and never the history.
- Inline simple helpers.
- Question whether functionality is needed at all before adding it.
- Deleting code is better than adding code.
- Prefer the simple solution.
- Say it once, in the fewest words that hold up.
- Suggest follow-up sessions when finished, and use an interactive prompt.
- Try to do stuff asynchronously. ex. ask about follow-ups while a lane runs.
- Stop when progress stalls.
- If the core problem is addressed, ship it.

# Development

PRs target `main`.
Before starting, `git fetch origin` and set the upstream to the base branch.

Use the Nix dev shell so tooling matches local lint. direnv loads it. Otherwise: `nix develop --command ...`.
The shell carries client toolchains. The moq binaries under test come from a channel.

```bash
just check        # shfmt, shellcheck, actionlint, freshness
just smoke        # rust-only matrix; flags pass through to smoke.sh
just full         # cross-language matrix
```

`just check` does not run the matrix. Run the lane you changed (`just smoke`, `just token`, `just cloudflare`, `just moxygen`, `just relays`).
