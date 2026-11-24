# [S] One broken NAR download no longer turns off nix.moq.dev for a CI job

## Goal

A NAR download from nix.moq.dev that breaks mid-stream costs a CI job that
one retry, not a minute of source builds. Nightly logs show no
`disabling binary cache 'https://nix.moq.dev'` line over a week of runs.

## Plan

Found 2026-11-24 while recording the bucket sizes for the Cachix retirement
([moq-dev/moq#8048](https://github.com/moq-dev/moq/pull/8048)). In the
Nightly runs of 2026-11-16, 11-17 and 11-19, one `ubuntu-24.04-arm` job each
(BBB rendition alignment, Rust coverage, test resume-model) failed to fetch
a large NAR: `nar/0vyq2c8m...nar.zst` (aarch64 `llvm-tools-preview-1.98.1`)
twice and `nar/1wmx8v36...nar.zst` (aarch64 `cargo-1.98.1`) once, each
ending `HTTP error 200 (curl error: Failure when receiving data from the
peer)` or `Failed sending data to the peer` after Nix's retries. Nix then
logged `disabling binary cache 'https://nix.moq.dev' for 60 seconds`, and
with `fallback = true` (`flake.nix`) that job built from source whatever it
needed in that window, `uniffi-bindgen-go` (and once `uniffi-bindgen-cpp`)
included. The other ~90 jobs in each run substituted the same paths, so the
objects had not expired: the 14-day expiry is not the cause. The
`nix-ci-moq-dev` bucket logged 993 `500 Internal Server Error` and 134
`429 Too Many Requests` responses over 2026-10-25 to 11-24 (R2 metrics), a
likely source of the broken streams.

- Find which hop drops the stream: the worker's observability logs for those
  timestamps, and R2's error metrics. The worker returns `object.body`
  straight through, so an R2 read failing mid-body truncates the response
  after its `200`.
- Fix it at that hop. Candidates: resume a failed R2 body with a ranged
  `get` from the bytes already sent; or serve `nar/` from an R2 custom
  domain without the worker. Raising Nix's `download-attempts` only retries
  the same failure.
- Nothing runs `infra/nix/src/worker.test.ts` today (no recipe or workflow
  calls `bun test` there); a worker change wires it into `just ci`. The
  worker's `bucket.get` handler also matches error message text
  (`/\((10001|10043)\)$/`), which `AGENTS.md` forbids; replace it if the fix
  touches that path.

Public API: none. Wire: none.
