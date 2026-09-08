# Commits

PRs are squash-merged, so the PR title becomes the commit subject and the PR description becomes the body in `git log`.

- Use conventional-commit subjects (`feat(watch): ...`, `fix: ...`, `chore: ...`, `docs: ...`)
- AI commit attribution goes in a `Co-Authored-By:` trailer, not the commit body.
- Never commit binaries or build artifacts (`.a`, `.so`, `.dylib`, `.dll`, wheels).

# PRs

Keep the body short and structured, not narrated.

- **Summary**: a few bullets on what changed and why. For a bug fix, state the root cause.
- **Public API**: every new/renamed/removed/updated exported item, with breaking ones called out.
- **Wire**: any change to the on-the-wire format, and the draft under `drafts/` updated with it.

When pushing additional commits to an existing PR, update the title and description if needed.
When taking over someone else's PR, push commits on top of theirs so they keep credit.

# CI

`Check` and `Test` compile the packages a branch changed and run their unit tests.
`Gates` is the behavioral half: it always starts, asks the impact map which end-to-end lanes the diff needs, runs those, and reports one result whatever was selected.

Ask for the same answer locally before pushing:

```bash
just gh select              # this branch's lanes, as `<lane>=true|false`
just test smoke-core        # what the `smoke` lane runs
```

| Lane | Runs | Selected by | Cost |
|---|---|---|---|
| `smoke` | `just test smoke-core`: rust and browser publish; rust, browser and C subscribe | any change reaching moq-relay, moq-cli, libmoq, moq-ffi or moq-gst through the dependency graph, a `js/` package, or the dev shell | ~10 min |
| `smoke_full` | `just test smoke-full` plus the negative control: every publisher against every subscriber | a change *to* the wire (moq-net), the FFI (moq-ffi, libmoq, moq-gst), a gateway, the python or Go client, or the bun workspace | ~20 min |
| `wasm` | `just test wasm`: the `@moq/wasm` bindings in headless Chromium | any change reaching moq-wasm or moq-relay, plus `js/wasm`, `js/net`, `js/signals`, `js/tsconfig.json`, `test/wasm`, `.cargo/config.toml`, the bun workspace, or the dev shell | ~8 min |
| `ts` | `just test ts`: the MPEG-TS exporter graded with TSDuck | any change reaching moq-mux or moq-cli, plus `test/ts` or the dev shell | ~5 min |
| `windows` | `just rs windows`: a compile gate, not a device test | an edit to moq-video, moq-audio, moq-nvenc, moq-transcode, moq-native or moq-cli | ~13 min, uncached |
| `macos` | `just rs macos`: same, for VideoToolbox and ScreenCaptureKit | an edit to moq-video or moq-audio | ~5 min, uncached |
| `features` | `just rs features`: the `--all-features` and `--no-default-features` permutations | a manifest, a build script, or the toolchain pin | ~20 min |

"The dev shell" is `flake.nix` and `flake.lock`, which supply ffmpeg, TSDuck, and the `wasm-bindgen` CLI every harness runs on; `windows` and `macos` use the runner's own toolchain instead, so nix never enters them.

Costs are wall clock on a cold shared cache, measured on the run that added this table; every selected lane runs in parallel, so a diff selecting all seven finishes in the slowest one. Selection itself costs ~90s, which every pull request pays.

The map lives in `.github/scripts/select.sh`, its fixtures in `select.test.sh`, and the aggregate in `gates.sh`. A lane is three things: an entry in the map, an output on gates.yml's `select` job, and a job whose id is the lane name. Miss one and `Gates` fails rather than passing quietly.

Deliberately still nightly, and so landing on `main` rather than in review:

- Swift, Kotlin, and Dart. The interop matrix has no client for any of them, so no aggregate result covers those bindings however green it is. Go is covered, but only by `smoke_full`.
- Feature-arm breakage that arrives through source rather than a manifest.
- A dependency-side API break reaching `#[cfg(target_os = ...)]` code, since the platform lanes key on the crate that holds it.
- The OBS link (`obs.yml`), the Swift package (`swift.yml`), `just rs audit`, and the TS exporter's live release timing.

# AI

AI-assisted issues, pull requests, reviews, and comments are welcome.
GitHub issues are the public front door for brainstorming. Prefer a quest for work needing durable scope or coordination.

Add the AI marker `(Written by <model>)` to any posts on GitHub, excluding commit messages that contain `Co-Authored-By:` trailers.

# Reviews

Codex and CodeRabbit review every push on their own. Never request a review; an @codex or @coderabbitai mention is banned.
CodeRabbit may be rate-limited, treat it as optional.

Fix the findings you agree with, reply to the ones you do not, and push once.
If the next automatic review still has findings, stop and report to the user.
If a finding is out of scope, make or update a follow-up quest.
