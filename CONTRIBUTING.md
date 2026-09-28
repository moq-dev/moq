# Commits

PRs into `main` are squash-merged, so the PR title becomes the commit subject and the PR description becomes the body in `git log`.
PRs into any other branch (`dev`, a questline) use a merge commit, so their history survives until they land.

- Use conventional-commit subjects (`feat(watch): ...`, `fix: ...`, `chore: ...`, `docs: ...`)
- AI commit attribution goes in a `Co-Authored-By:` trailer, not the commit body.
- Never commit binaries or build artifacts (`.a`, `.so`, `.dylib`, `.dll`, wheels).

# PRs

Keep the body short and structured, not narrated.
Have at least these sections:

- **Problem**: a summary of the problem and why this PR is needed.
- **Approach**: a summary of the approach taken to solve the problem.
- **Impact**: a bullet point for every public API/wire change made.
- **Alternatives**: any alternative approaches considered.
- **Follow-ups**: any issues encountered or quests created.

When pushing additional commits to an existing PR, update the title and description if needed.
When taking over someone else's PR, push commits on top of theirs so they keep credit.

Create a draft PR.
Switch it to "Ready for review" when you're finished and local `just check` passes.
Fix any merge conflicts and failing CI checks.

# AI

AI-assisted issues, pull requests, reviews, and comments are welcome.
Especially bug reports; dive deep into the root issue before proposing a solution.

GitHub issues are the public front door for brainstorming.
Prefer a quest for work needing durable scope or coordination.

# Reviews

AI agents review every push on their own.
Never explicitly request a review.

Codex reacts with thumbs up if there are no findings.
CodeRabbit may be rate-limited, treat it as optional.

For each finding:

- If you don't agree with it, reply to the finding and move on.
- If it's a relatively easy improvement, fix it and push. Update the summary if needed.

Wait for Codex to review the final head before merging.
Merge only on its thumbs up, or once every Codex finding on the PR is fixed or replied to.
Codex skips fork PRs; ask the maintainer to request one.

# CI

Workflow steps run `just` recipes, never a script path; `just gh check` enforces it.

# Follow-ups

If you encounter issues, or findings that are out of scope, create follow-up quests.
Focus on the core problem, offering a potential solution only if its obvious.
For non-trivial tasks, file an issue or offer to run `/quest-plan`.

# Forks

[moq-dev/noq](https://github.com/moq-dev/noq) publishes `moq-noq*`, the QUIC stack every published MoQ crate builds on; iroh keeps upstream noq.
Its `moq-sync` workflow merges n0-computer/noq weekly as a PR; review it like any other, and `PARENT` names the upstream commit each release includes.
A carried change lists its upstream PR, or the reason it has none, in the fork PR.
For an advisory against noq or Quinn, compare the pinned release's `PARENT` with the fixing upstream commit, then sync, release the fork, and bump the pin here.

# Versions

Releases are cut separately; bump only when asked. Each package's version lives in one place:

- **Rust**: release-plz owns crate versions and Rust dependency requirements.
- **JavaScript**: `js/*/package.json` packages with a `scripts.release` entry (skip private ones like `@moq/clock`, `@moq/wasm`), plus the matching workspace version in `bun.lock`.
- **Python**: `py/moq-rs/pyproject.toml`, plus the matching `moq-rs` entry in the root `uv.lock`; `py/moq-ffi` follows the `moq-ffi-v*` tag and Rust crate.
- **Swift**: `swift/VERSION`. **Kotlin**: `moq.version` in `kt/gradle.properties`. **Dart**: `version` in `dart/moq/pubspec.yaml`. Their FFI counterparts track the Rust crate.
- **Go**: `go/wrapper/VERSION` holds a human-owned `MAJOR.MINOR` line; CI derives the patch, so only edit it for a breaking API. Leave the placeholder FFI version in `go.mod` alone.
- **OBS**: release builds take their version from the `libmoq-v*` tag that release-plz cuts for the `libmoq` crate (`cpp/obs/build.sh --libmoq-release`), not from any manifest, so there is nothing to bump.
