# [S] moq-archive's cross-backend recording test survives load

## Goal

`moq-archive producer::backends::recordings_are_byte_identical_on_every_backend`
(`rs/moq-archive/src/producer/backends.rs`) passes reliably in a loaded
merge-queue `just ci`, fixed at its cause, never by raising a timeout or
adding a retry. Done when the test no longer fails the queue and a loaded
local repeat of it passes.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`). It failed the merge-queue
batch for [moq-dev/moq#7754](https://github.com/moq-dev/moq/pull/7754)
([run 41760215336](https://github.com/moq-dev/moq/actions/runs/41760215336),
2026-11-04 02:21Z) after 2.05 s, on the S3 backend:

```text
FAIL [   2.053s] (  819/10037) moq-archive producer::backends::recordings_are_byte_identical_on_every_backend
thread 'producer::backends::recordings_are_byte_identical_on_every_backend' (1814852) panicked at rs/moq-archive/src/producer/backends.rs:198:5:
assertion `left == right` failed: S3
```

Comparing the two maps in the assertion shows only the video objects
differed. The in-memory recording stored groups 2–3 in object `…02-…-…2000`
and groups 4–5 in `…04-…-…4000`. The S3 recording stored group 2 alone in
`…02`, then groups 3–4 in `…03`, then group 5 alone in `…05`.

A likely cause is in the test's wait rather than the producer. `record`
aborts the first run once the `…02` object exists (`stored` only `head`s the
path). With a 200 ms flush, a slow S3 put can leave that object holding only
group 2 when the abort lands. The second run then correctly resumes after the
last stored group (2), so the split differs from the backends that stored
groups 2 and 3 together.

- Confirm it, for example by delaying the S3 mock's puts.
- If confirmed, make `record` wait for the stored object to hold both groups
  (decode it) before aborting, not just for it to exist. If the producer is
  wrong to flush a partial object there, fix the producer instead.
- [moq-dev/moq#7752](https://github.com/moq-dev/moq/pull/7752) edits this
  file. Start from whichever version is on main.

Follow the rules in [More tests under load](/quest/m0/test-flakes-2/README.md).
