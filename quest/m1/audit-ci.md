# [M] CI workflow audit

## Goal

Every CI workflow and job earns its place: its results reach someone (a gate,
a Discord alert, or a published report) at a cost proportional to that signal.
Each one that doesn't is fixed, moved, or deleted. Individual tests stay with
the [test cost audit](/quest/m1/audit-test-cost.md), and runner and cache
infrastructure with [CI cost and latency](/quest/m0/ci/README.md).

## Plan

Recurs once per major release (decided 2026-10-20). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2), never m3, the deferred bucket
that does not shift with releases. The successor keeps the Goal and this
paragraph; the rest of its Plan is rewritten with what this run learned: the
per-workflow table, baselines, and what to check first. A successor placed in m2 lists the
[release cut](/quest/m2/release/cut.md) under `Required`, so it waits for the
release in flight to ship. The parent milestone lists this quest under
`Required`. Only `/quest-delete` ends the recurrence. Fixes land in the run's
PR, one commit each, rather than as follow-ups. Only a change that needs a
maintainer decision (dropping a release target, scope) or is independently
landable and too large to review alongside the rest becomes a follow-up
through `/quest-plan`.

Split from the test cost audit on 2026-10-20: this audit owns workflows and
jobs, that one owns tests. It also owns job-level build cost, which the
one-time m0 CI pipeline audit handed over on 2026-10-21 after measuring the
one-pass queue.

Each run tables every workflow in `.github/workflows/` and every Nightly job
with three verdicts:

- **Signal:** what regression it catches that nothing cheaper catches, and
  when it last caught one.
- **Visibility:** who sees a failure or a result. A gate blocks merging, a
  scheduled or publishing workflow posts to Discord `#alerts` through
  [alert.yml](/.github/workflows/alert.yml) (`just gh check` enforces its
  coverage), and a report lands on the
  [Pages site](https://git.moq.dev/) or in a job summary. A result
  only in a run artifact or log is invisible: publish it, summarize it, or
  stop producing it. Each report on the Pages site owns one `gh-pages`
  directory: its workflow commits only that directory, deploys the whole
  branch, and joins the `benchmark-pages` concurrency group with
  `queue: max`, so no deploy drops another report (decided 2026-10-20; the
  benchmark, relay, and interop reports do, through `sh/gh/pages-publish.sh`).
- **Cost:** runner-minutes per month by runner type, from the org billing API
  and job timings, against that signal. macOS is the scarce one (a 5-job
  concurrency cap).

Check first (from the test cost audit's m0 run, 2026-10-19):

- Classify every failed job of the latest Nightly, scheduled Interop, and
  Platform runs, not just the slowest. On 2026-10-19, 28 Nightly jobs had
  failed for eight independent causes, some for days. A red Nightly hides the
  next regression: the send-depth gate and all six fuzz targets broke on
  2026-10-18 and stayed broken unnoticed until that audit. Nightly was still
  red on 2026-10-20 (fuzz, media players, audit, features, release builds), so
  an alert that always fires is not visibility either.
- Platform (macOS, Windows) runs only in the merge queue and is not required,
  so a Windows-only compile error in moq-video stayed red for two days across
  dozens of queue runs until
  [#5966](https://github.com/moq-dev/moq/pull/5966). Most Platform failures
  in the sample were that error, a choco `nasm` install outage, or another
  queued PR's breakage. Rank Platform by failure cause, not count.
- A queue failure often belongs to another PR in the same batch: a stale
  fixture in the entry itself, or a semantic conflict with a PR merged after it
  was queued. Check the entry's head branch and what merged around it before
  calling it a flake.
- Owned elsewhere: Rust advisories by
  [the dependency audit](/quest/m1/audit-deps.md). The release toolchain now
  links.
- Re-running a single job in place cannot redeploy Pages: the earlier
  attempt's `github-pages` artifact remains and `deploy-pages` refuses two.
  Delete the stale artifact first, or wait for the next Nightly.

Check first (from the m0 CI pipeline audit, 2026-10-21):

- The queue runs one `just ci` job on the persistent `nix/runner` VM. Over 32
  entries it took a median 5.5 minutes, p90 11.9, at most 24.2. Its job
  summary lists seconds per module, and the `cargo-timings` artifact is the
  per-crate drill-down behind that table. Re-measure from those, not from log
  timestamps.
- An `rs/moq-ffi` change selects every binding, and the bindings were about
  half of the slowest entries: C++ with the OBS plugin about 4.5 minutes
  (the generated `moq.cpp` compiles six times at `-O0`, once per consumer
  build and standard), Dart 45 to 160 seconds, Kotlin 30 to 110 (a cold
  Gradle daemon is 65), Go about a minute. The dispatcher runs modules one
  after another, and most binding steps use one core of eight.
- A core Rust change (moq-net, moq-quic, moq-tokio) spends 3 to 4.5 minutes
  compiling the test build and 2 to 2.5 running it, plus separate builds for
  `cargo doc` (25 to 60 s), the io_uring clippy (20 to 50 s), the wasm32
  clippy (12 s), and the capture (30 to 36 s) and play (40 to 48 s) tests.
- Deferred to the maintainer (coverage trade-off): fold the capture and play
  test builds into the main test build, as the platform build already does
  (about 75 s per core Rust entry; the non-feature arms of moq-video,
  moq-audio and moq-cli would compile only in Nightly's Features job, and
  openh264 in `dev` tests needs measuring first), or move both to Nightly
  (the same saving, but the gate stops testing the device features). The
  m0 audit recommended folding; the queue keeps both builds until decided.
- Cold builds (release PRs, Nightly) still compile both rustls backends and
  aws-lc-sys once per environment; the queue VM keeps those built.
- Nightly on 2026-10-20 took about 1,450 runner-minutes (320 macOS) and was
  red for ten causes. Features' clippy is fixed except moq-net's group bench
  ([#6117](https://github.com/moq-dev/moq/pull/6117)), and `gh test` was fixed
  by [#6086](https://github.com/moq-dev/moq/pull/6086). Unowned:
  `rs audio-devices`, new on 2026-10-20, fails all five playback tests on the
  hosted ARM runner and passes on x86_64 Linux. Owned elsewhere besides the
  above: the Rust and JS benchmark legs by the
  [performance audit](/quest/m0/audit-perf.md), Audio quality's budgets by
  the [audio jitter target](/quest/m1/audio-jitter-target/README.md), and the
  bun advisories with the Rust ones.
