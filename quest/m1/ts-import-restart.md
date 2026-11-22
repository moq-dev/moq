# [L] A TS export loses only what a restarted import missed

## Goal

When `import ts` is killed and restarted under a running `export ts`, the
export resumes on its own. It loses no more programme than the import missed
while down, plus the restarted import's first group. On `main` it loses 14.4 s
on loopback. A `just test ts --passthrough --restart <s>` arm asserts that
bound.

## Plan

Proposed in [#9315](https://github.com/moq-dev/moq/issues/9315) (P2-t of the
reporter's T86 evaluation). The source plays live into a local UDP port, so a
killed import loses only what it held. A loop restarts the import 0.5 s after
it exits, while the export runs on.

Decided 2026-11-21 (maintainer, /quest-import of #9315):

- ✅ This quest fixes whatever exceeds the bound, not just measuring it
  (rejected: a test-only arm that reports the gap, which would leave the
  14.4 s loss unowned).
- ✅ The bound is the import's downtime plus its first group (rejected: the
  reporter's export `--delay` plus first group). The delay is buffer, not
  loss, so adding it would hide up to several seconds of real loss.

Find where the 14.4 s goes before fixing anything: the import's restart, the
broadcast going live again, the export noticing and resubscribing, or its
start judgement on the new broadcast. Learn of the new broadcast through its
announcement, not a timer. The arm reports the gap in seconds of the clip
(`passthrough.py`), the export's exit status, and whether it resumed without
being restarted, then asserts the bound.

`test/ts` runs daily and in the release gate, not the merge queue
(`.github/workflows/ts.yml`). A timing-sensitive arm stays there.

## Closes

- [#9315](https://github.com/moq-dev/moq/issues/9315) - three arms for just test ts

## Related

- [TS harness late join to one program](/quest/m3/ts-harness-program-join.md) - another arm from #9315
