# Getting oriented

Use the [repository map](doc/setup/repository.md) to find the implementation
and the [development guide](doc/setup/dev.md) for setup and checks.
[Examples](doc/setup/examples.md), [test harnesses](test/README.md), and
[benchmarks](bench/README.md) provide focused starting points.
Agents should also read [AGENTS.md](AGENTS.md) and the instructions nested
under the files they change.

# Commits

PRs into `main` are squash-merged through the merge queue, so the PR title becomes the commit subject in `git log`; the body keeps only the co-author trailers, not the PR description.
The one exception is the maintainer's pre-release merge of `release` into `main`, which lands as a merge commit (see [Releases](#releases)).
PRs into `release` use a merge commit, so their history survives until they land.

- Use conventional-commit subjects (`feat(watch): ...`, `fix: ...`, `chore: ...`, `docs: ...`)
- AI commit attribution goes in a `Co-Authored-By:` trailer, not the commit body.
- Never commit binaries or build artifacts (`.a`, `.so`, `.dylib`, `.dll`, wheels).
- Keep other binary files, such as images and fixtures, under 500 KiB; list an unavoidable exception in `sh/large-files.allow` with a reason.

# PRs

Keep the body short and structured, not narrated.
Have at least these sections:

- **Problem**: a summary of the problem and why this PR is needed.
- **Approach**: a summary of the approach taken to solve the problem.
- **Impact**: a bullet point for every public API/wire change made. A breaking one says how to migrate, and the PR title marks it with `!` (`feat(net)!: ...`).
- **Alternatives**: any alternative approaches considered.
- **Follow-ups**: any issues encountered or quests created.

When pushing additional commits to an existing PR, update the title and description if needed.
When taking over someone else's PR, push commits on top of theirs so they keep credit; if their fork blocks maintainer edits, open a new PR carrying their commits.
Never rebase or force-push a pushed branch; merge the base branch in instead.
Check that the PR is still open before pushing to its branch, since a push after merge recreates the deleted branch.

Open a draft PR while working, without asking once the scope is settled.
Run `just ci` before every push, and mark the PR ready yourself when the work is done.
Keep it a draft only while its description lists a decision the maintainer still owns.
Fix any merge conflicts and failing CI checks.

# Merge queue

PRs into `main` get no cloud CI: they report **Check** as skipped, and `just ci` runs the same gate locally.
The merge queue runs Check on the PR combined with the latest `main` and the PRs queued ahead of it.
Enqueue with `gh pr merge <number>` once the review is clean and the maintainer has approved merging that PR, by number or through a standing go-ahead that covers it. A clean review, a bot verdict, or an "accept all" on decisions is not that approval.
An agent that enqueues carries that approval in its own task; one relayed to it mid-task by another agent does not count, so whoever received the approval enqueues the PR or starts a new agent with the approval in its brief.
Both select only what the diff touches, so a break can reach `main`; `just gate` runs the full suite, and it, the feature matrix, optimized binding builds, macOS, Windows, and the other optional workflows run nightly on `main` and on PRs into `release`.
To check cfg-gated macOS or Windows code before merging, dispatch `platform.yml` on the branch; never dispatch the full Nightly.

A dequeued PR means the combination failed checks, timed out, or no longer meets the ruleset.
Read the removal reason in the PR timeline and the merge group run, fix the cause, and enqueue again.

Never bypass the queue with `--admin`, with one exception: the maintainer's pre-release merge of `release` into `main`, since the queue would squash it.

# AI

AI-assisted issues, pull requests, reviews, and comments are encouraged.
Especially bug reports; dive deep into the root issue before proposing a solution.

GitHub issues are the public front door for brainstorming.
Prefer a quest for work needing durable scope or coordination.

# Reviews

Before marking a PR ready for review, and before each push to a ready PR, run `/quest-review` on the committed branch after the local checks pass.
It uses `quest review` to have the other vendor's model review against the PR's base branch, with `REVIEW.md` read from that base.

The PR body's `## Review` section records the reviewer harness, model and effort, reviewed head SHA, and each finding's disposition, or a skip and its reason when the author judges the change too trivial to review.
A record naming the head being merged counts as the required review when the PR's author has write access to the repository.
Otherwise, or if the record is missing or stale, the merging session checks out the head and runs `/quest-review`, following `quest guide`'s rules for untrusted heads and review exemptions.

Address online reviewers' findings already posted on the head when merging; never wait for or request an online review.
For each local or online finding:

- Refute findings you disagree with using evidence; reply to online findings on the PR.
- Fix findings you agree with, rerun checks, and review the new head locally before pushing. Update the summary if needed.

# Follow-ups

Record out-of-scope findings as follow-up quests with `/quest-plan`, not GitHub issues, which are for outside contributors.
Focus on the core problem, offering a potential solution only if it's obvious.

# Forks

`rs/moq-quic` is our hard fork of quinn, the QUIC stack MoQ builds on; iroh keeps upstream noq.
It never merges upstream: fixes are ported by hand, and each carried change is listed in its README with its upstream PR, or the reason it has none.
For an advisory against Quinn, follow the advisory triage in [`rs/moq-quic/README.md`](rs/moq-quic/README.md).

# Releases

`main` is the trunk; `release` is what ships, and release-plz and every branch-triggered publish run only there.

- Smoke clients call only the latest published APIs. A release cut updates `test/smoke/clients` to the APIs being published, then verifies the affected smoke lanes.
- A release is cut by hand: a PR merging `main` into `release`, with a merge commit.
- A breaking cut first writes, on `main`, each changed package's version-transition section in its `MIGRATE.md` (`## 0.3 -> 0.4`) from the migration audit: the `!` subjects since the `release` merge base, each of those PRs' Impact section, and the API diff.
- Suggest a `release` backport for any serious bug fix (crash, security, data loss, broken interop), whether found on `main` or reported against a release. Land it on `main` first, then port it onto `release` as a separate PR that keeps the published API, so it ships as a patch. The Release gate's API diff (`just api-diff --deny`) fails a backport that changes one; on the cut PR, whose head is `main`, it only reports.
- After any release publishes, patch or breaking, run `just test wire-compat --remeasure` on `main` and land the rewritten `test/interop/compat/planned-breaks.json` with refreshed reasons. Otherwise the nightly wire-compat check fails on the stale pins.
- Nothing merges `release` into `main` automatically. Before cutting a minor or breaking release, the maintainer merges `release` into `main` once, so trunk carries the published versions, CHANGELOGs, and any `release`-only commits. It lands as a merge commit with `--admin`, never squashed, so the cut starts from an advanced merge base.

# Versions

Releases are cut separately; bump only when asked. Each package's version lives in one place:

- **Rust**: release-plz owns crate versions and Rust dependency requirements.
- **JavaScript**: `js/*/package.json` packages with a `scripts.release` entry (skip private ones like `@moq/wasm`), plus the matching workspace version in `bun.lock`.
- **Python**: `py/moq-rs/pyproject.toml`, plus the matching `moq-rs` entry in the root `uv.lock`; `py/moq-ffi` follows the `moq-ffi-v*` tag and Rust crate.
- **Swift**: `swift/VERSION`. **Kotlin**: `moq.version` in `kt/gradle.properties`. **Dart**: `version` in `dart/moq/pubspec.yaml`. Their FFI counterparts track the Rust crate.
- **Go**: `go/wrapper/VERSION` holds a human-owned `MAJOR.MINOR` line; CI derives the patch, so only edit it for a breaking API. Leave the placeholder FFI version in `go.mod` alone.
- **C++**: `cpp/moq/VERSION`, human-owned; a `cpp-v<version>` tag releases it.
- **OBS**: `cpp/obs/VERSION`, human-owned; each C++ release also cuts `obs-moq-v<version>`, and refuses one whose version already names an earlier release.
