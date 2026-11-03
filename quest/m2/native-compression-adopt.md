# [M] Adopt native browser codecs when the frame contract holds

## Goal

Use native browser DEFLATE operations in place of bundled JS code wherever
they preserve MoQ's frame availability, dictionary reuse, synchronous
behavior and resource bounds. Keep the approved codec until those conditions
are met; the current compression feature does not wait for browser changes.

## Plan

Decided with the maintainer on 2026-10-21: native adoption follows the current
implementation. The interim implementation keeps synchronous writes and
accepts the measured approximately 2.9 KB gzip static encoder increase.
Do not silently replace that decision with pending asynchronous writes,
main-thread blocking through workers, or a new preparation API.

- Check current browser APIs and the linked upstream discussions before
  starting implementation. Constructor support alone is insufficient.
  Check nonfinal frame output, shared history, exact declared-size admission,
  early output-overrun refusal, cancellation, malformed input and group end.
- Native decoding is also an avenue in the size audit. JS compression landed
  with a bundled pako codec in `js/net/src/flate`; coordinate with the audit
  and remove only duplicated conditional scope. Encoder and decoder adoption
  may land independently if each preserves the existing contract and removes
  worthwhile bundled code.
- Chromium 156 decoded the tested nonfinal frames before closure; this does
  not establish synchronous availability, cross-engine behavior or bounded
  allocation. Verify those properties rather than treating the experiment
  as approval to replace the decoder.
- Preserve the moq-flate wire format. Native stream finalization may need a
  local final block for validation; it must not change transmitted bytes or
  hide truncation. Retain tests for independent fresh windows per group and
  shared history between frames.
- Measure the same real consumer and publisher bundles as the existing size
  tooling and benchmark frame latency and fanout. Wire the selected browser
  regressions into CI. Feature detection must select only supported behavior.
- If the required native operation is still unavailable, record its current
  issue/status and leave this condition quest open. Any API or wire course
  change needs a new maintainer decision. Update affected docs inline;
  the compression guide owns the user-facing entry point, not a new page.

## Related

- [Frame flush report](/quest/m2/native-compression-flush.md) - the native nonfinal encoder flush gap
- [Synchronous codec report](/quest/m2/native-compression-sync.md) - the stateful synchronous API gap
