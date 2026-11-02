# [S] Re-measure interleaved fMP4 import

## Goal

The 4-track, 16-run, whole-input `cmaf-interleaved-import` benchmark is no
slower than before the streaming change, or the cost is measured and accepted.
The quiet-host comparison remains outstanding.

## Plan

The framing part is implemented in this checkpoint: a decode error is terminal,
and later decodes and controls return the first error without reading input or
changing tracks. This
removes recovery/skip state instead of draining arbitrary input after an error.
`finish()` still ends accepted tracks without resetting the error. HLS retires
a refused importer immediately and replaces it on the next pass, while transient
fetch failures keep their retry budget. CLI chunked input
propagates a decode error and aborts. MKV leaf limits remain independent.

The original decision (maintainer, 2026-10-31, /quest-complete requeue) allowed
either draining the refusing call or terminal refusal, whichever kept the state
simpler while meeting HLS needs. It came from
[moq-dev/moq#7232](https://github.com/moq-dev/moq/pull/7232) (fMP4 streams mdat),
[moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234) (MKV streams blocks),
and [moq-dev/moq#7468](https://github.com/moq-dev/moq/pull/7468#issuecomment-6554670247).

- Compare with `038ecfa6b3c69cabf55a7b2bb975c566381ccb3b`, the parent of the
  streaming change. The original possible ~25% slowdown came from a noisy run
  whose 4-track cases swung ±40% between repeats of the same source.
- This attempt's host is `desktop`, Linux x86_64, 24 allowed logical CPUs.
  On 2026-11-02 at 11:53 UTC its load averages were 7.28 / 3.75 / 3.18,
  with other agents active. This does not satisfy the quiet-host prerequisite.
  Do not treat shared-host timing as acceptance or wait indefinitely for quiet.
- A diagnostic comparison on that shared host used three alternating rounds,
  1 s warmup, 2 s measurement and 30 samples, pinned to performance CPUs 0-7.
  The historical whole-input ID lacks today's `-4232444chunk` suffix, so the
  matching cases were paired explicitly rather than trusting the harness's
  initial "new case" result. Base/current means in milliseconds were
  7.992/1.037, 7.539/1.066 and 6.373/1.034. One-minute load ranged from
  2.42 to 5.44 with other agents and local checks active. These are full-revision
  timings, not an isolated attribution to streaming or acceptance of the cost.
- Re-run the paired comparison on a quiet host, then fix a confirmed regression
  or ask the maintainer to accept the measured cost.
- Inherited-environment `just ci` hit mbx `Argument list too long (os error 7)`
  in Rustdoc and the Clippy-gate test. The authorized clean-shell command,
  `quest gate nix develop --ignore-environment --keep HOME --keep USER --keep PATH --command just ci origin/main`,
  passed all selected checks while retaining mbx. No launcher bypass or upstream
  mbx change is part of this checkpoint.

Public API: decode errors are now terminal for decoding and controls, including
through `import::Container`;
replace the importer to resume. Signatures and wire formats are unchanged.

## Related

- [MKV leaf element limits](/quest/m1/mkv-leaf-limits.md) - independent importer work
- [Media fuzz](/quest/m2/media-fuzz.md) - coverage for dropped-input bugs
