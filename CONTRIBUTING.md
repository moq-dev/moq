# Contributing

How a change lands in this repo: branch targeting, commits, PR descriptions, reviews, and releases. Coding conventions live in [CLAUDE.md](CLAUDE.md) and the per-directory guides; this file covers the mechanics around the code.

## Branch Targeting

Two long-lived branches. The split is about **semver breakage, not size or novelty**: `dev` is only for changes that break an existing published contract. Everything else (bug fixes, new behavior, new/additive APIs, docs, refactors) goes to `main`, however large.

- **`main`**: the default. Bug fixes, new behavior, new/additive APIs, docs, and refactors that preserve the existing public/wire contract. A change that only *adds* is additive and lands here even when it is big: a new `pub` item, a new option, or a parser accepting a broader set of inputs it previously rejected. Changing what a component does with input it *already* takes (e.g. recognizing a media pattern it used to mishandle) is a fix, not a break, so it also lands here.
- **`dev`**: reserved for changes that violate semver by breaking an existing contract. Target it only for:
  - Wire-protocol changes (anything under `rs/moq-net`, including `moq-lite` / `moq-transport` framing or draft bumps).
  - Breaking changes to public APIs in `rs/moq-ffi`, `rs/libmoq`, `rs/moq-net`, `rs/hang`, `js/net`, `js/hang`, or any of the language wrappers under `swift/`, `kt/`, `go/`, `py/`. This means a renamed, removed, or signature-changed `pub` item, not a newly *added* one (adding is additive, so it goes to `main`).
  - Catalog/container format changes in `rs/hang` or `js/hang` that alter existing on-the-wire framing or fields.

`dev` periodically merges into `main` (or vice versa) when the batch is ready to ship. When in doubt, target `main`; reviewers will redirect to `dev` if a change turns out to break an existing contract. CI (`pull_request:` workflows) runs on PRs against either branch, so no extra setup is needed when you switch the base.

## Commit Messages

PRs are squash-merged, so the PR title becomes the commit subject and the PR description becomes the body in `git log`. Write both for that reader.

- Use conventional-commit subjects (`feat(watch): ...`, `fix: ...`, `chore: ...`, `docs: ...`); release-plz derives crate changelogs from them.
- AI commit attribution goes in a `Co-Authored-By:` trailer, not the commit body.
- Never hand-bump a `version =` field in a feature PR. release-plz owns Rust version bumps; a manual bump breaks the Release RS workflow. (`py/moq-rs` is the exception: its version is bumped by hand.)
- Never commit binaries or build artifacts (`.a`, `.so`, `.dylib`, `.dll`, wheels). Release artifacts flow through GitHub Actions to mirror repos or Release assets.

## PR Descriptions

Keep the body short and structured, not narrated:

- **Summary**: a few bullets on what changed and why. For a bug fix, state the root cause (see Root Cause First in [CLAUDE.md](CLAUDE.md)).
- **Public API changes**: every new/renamed/removed/signature-changed `pub` item in `rs/moq-*` and `js/*`, with breaking ones called out per [Branch Targeting](#branch-targeting). Distinguish genuinely public surface from `pub(crate)`/private.
- **Test plan**: what was run and verified.
- If you skip a [Cross-Package Sync](CLAUDE.md#cross-package-sync) row, say why.

Skip file-by-file narration of the diff; the diff already says that.

### Keep the title and description fresh

When pushing additional commits to an existing PR, check whether the title and description still describe the change accurately. They often go stale during review iterations: a flag gets renamed, an API gets reshaped, an extra fix lands. A stale title/body means a misleading entry in `git log` forever. Update with `gh pr edit <num> --title "..." --body "..."` whenever the scope shifts, watching for:

- Flags, file names, or public APIs renamed in later commits but still referenced by their old name in the body.
- Summary bullets describing behavior the latest commits have changed or removed.
- The test-plan checklist lagging behind newly added tests.

## AI Attribution

Every piece of LLM-authored prose posted to GitHub ends with the agent model, e.g. `(Written by GPT-5)`. That covers PR descriptions, issue bodies, review summaries, review replies, and any comment on a PR, issue, or discussion. Keep the marker when editing a body you authored, so readers still know it wasn't human-written.

The marker never goes in the codebase itself: no code comments, doc comments, or `/doc` pages (see Comment Conventions in [CLAUDE.md](CLAUDE.md)). GitHub prose is read once, in context, by someone deciding how much to trust it; source markers just rot in place. Commits are the other exception: attribution belongs in a `Co-Authored-By:` trailer, not a marker in the body.

## Reviews

CodeRabbit reviews PRs automatically, but it has an hourly quota and runs out of org credits. If a PR shows a "Review limit reached" / "out of usage credits" message instead of an actual review (or CodeRabbit otherwise fails to produce one), run the `/review` skill locally against the PR. Then act on the findings the same way you would CodeRabbit's: push the high-confidence, unambiguous fixes directly, and escalate anything ambiguous, architectural, or open to interpretation by asking first rather than guessing.

When reviewing a PR, always include the same public API changes list described above, and call out anything breaking per [Branch Targeting](#branch-targeting).

## Releases

- **Rust**: release-plz opens release PRs and publishes to crates.io on merge to `main` (`release-rs.yml`). `moq-relay` and `moq-cli` take patch bumps even for breaking changes (no external consumers yet). `moq-cli` is the one crate bumped by hand, in a dedicated chore PR rather than a feature PR, since release-plz can't see CLI surface changes.
- **JS**: `release-js.yml` publishes `@moq/*` packages (per-package build + `common/release.ts`).
- **Python**: `moq-ffi` releases on `moq-ffi-v*` tags; `moq-rs` publishes on merge to `main` when its hand-bumped version isn't already on PyPI. See `py/CLAUDE.md`.
- **Binding mirrors**: CI mirrors the `swift`/`kt`/`go` source skeletons to `moq-dev/moq-{swift,kotlin,go}` on each `moq-ffi-v*` tag.
