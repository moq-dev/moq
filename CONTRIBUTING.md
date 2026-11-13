# Getting oriented

Use the [repository map](doc/setup/repository.md) to find the implementation
and the [development guide](doc/setup/dev.md) for setup and checks.
[Examples](doc/setup/examples.md), [test harnesses](test/README.md), and
[benchmarks](bench/README.md) provide focused starting points.
Agents should also read [AGENTS.md](AGENTS.md) and the instructions nested
under the files they change.

# Commits

PRs into `main` are squashed through GitHub's merge queue, so the PR title becomes the commit subject in `git log`.
The one exception is the maintainer's pre-release merge of `release` into `main`, which lands as a merge commit (see [Releases](#releases)).
PRs into `release` use a merge commit, so their history survives until they land.

- Use conventional-commit subjects (`feat(watch): ...`, `fix: ...`, `chore: ...`, `docs: ...`)
- Never commit binaries or build artifacts (`.a`, `.so`, `.dylib`, `.dll`, wheels).
- Keep other binary files, such as images and fixtures, under 500 KiB; list an unavoidable exception in `sh/large-files.allow` with a reason.

# PRs

Keep the body short and structured, not narrated.
Have at least these sections:

- **Problem**: a summary of the problem and why this PR is needed.
- **Approach**: a summary of the approach taken to solve the problem.
- **Impact**: a bullet point for every public API/wire change made. A breaking one says how to migrate, and the PR title marks it with `!` (`feat(net)!: ...`). A behavior change alone takes no `!`.
- **Alternatives**: any alternative approaches considered.
- **Follow-ups**: any issues encountered or quests created.

When taking over someone else's PR, push commits on top of theirs so they keep credit; if their fork blocks maintainer edits, open a new PR carrying their commits.

Run `just ci` before every push, and mark the PR ready yourself when the work is done.

# Landing gate

**Check** runs only on PRs into `release`, including drafts. PRs into `main` get their verdict from the merge queue; run `just ci` locally for a signal before queueing. **Queue** reports as skipped until queued.
`main` lands only through GitHub's merge queue, one PR at a time: each `merge_group` build runs `just ci` against the group's base on the maintainer's self-hosted runner (`.github/workflows/queue.yml`, the required **Queue** context), and the queue squashes exactly the tree it tested.
Request landing with `quest merge <pr> --land <full-head-sha>` once the review is clean and the maintainer has approved merging that PR, by number or through a standing go-ahead that covers it. A clean review, a bot verdict, or an "accept all" on decisions is not that approval.
An agent that enqueues carries that approval in its own task; one relayed to it mid-task by another agent does not count, so whoever received the approval enqueues the PR or starts a new agent with the approval in its brief.
`quest merge --land` enqueues the pinned head, or arms auto-merge bound to it while its checks are pending; a push removes it from the queue. A head GitHub refuses to enqueue, for a conflict or failing checks, fails the command: fix it and request landing again.
Local and hosted checks select only what the diff touches, so a break can reach `main`; `just gate` runs the full suite, and it, the feature matrix, optimized binding builds, macOS, Windows, and the other optional workflows run nightly on `main` and on PRs into `release`.
Repository tooling tests run whole files in `just gh test`, listed explicitly in
`.github/justfile`. Slow fixtures live in separate `*.slow.test.py` files;
`just gh test-integration` runs those files and selector timing nightly.
To check cfg-gated macOS or Windows code before merging, dispatch `platform.yml` on the branch; never dispatch the full Nightly.

A PR the queue drops for a failing build or a conflict gets a fixer, which hands the PR back, with its diagnosis, when the fix needs a decision or stops making progress. `quest status` shows why a PR left the queue under `landing-stopped`.

Never bypass the queue, with one exception: the maintainer's pre-release merge of `release` into `main`, since the queue would squash it.

# AI

AI-assisted issues, pull requests, reviews, and comments are encouraged.
Especially bug reports; dive deep into the root issue before proposing a solution.

GitHub issues are the public front door for brainstorming.
Prefer a quest for work needing durable scope or coordination.

# Reviews

A `## Review` record naming the head being merged counts as the required review when the PR's author has write access to the repository.
Otherwise, or if the record is missing or stale, the merging session checks out the head and runs `/quest-review`, following `quest context`'s rule for untrusted heads and `/quest-merge`'s review exemptions.

# Follow-ups

Focus on the core problem, offering a potential solution only if it's obvious.

# Forks

`rs/moq-quic` is our hard fork of quinn, the QUIC stack MoQ builds on.
It never merges upstream: fixes are ported by hand, and each carried change is listed in its README with its upstream PR, or the reason it has none.
For an advisory against Quinn, follow the advisory triage in [`rs/moq-quic/README.md`](rs/moq-quic/README.md).

# Releases

Releases are cut by hand from `release`; see [RELEASE.md](RELEASE.md). Bump versions only when asked.
