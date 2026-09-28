# [S] lite-07 subscribers settle on the stream count

## Goal

On moq-lite-07, Rust and JS subscribers stop waiting for a subscription's tail
once they have read the headers of as many group streams as SUBSCRIBE_END
counts, so a group the publisher skipped or never opened costs no grace. A
late stream below the end is still accepted within the grace, which stays for
a stream reset before its header arrived. lite-05 and -06 keep the DROP
accounting JS already has and Rust track tail adds.

## Plan

- Rust and JS subscribers now settle on the received header count for lite-07.
  lite-05 and -06 keep their range and DROP accounting. The grace still covers
  counted streams reset before their headers arrive.
- Local Rust and JS regressions cover late streams, missing/reset streams,
  skipped sequences, and zero streams. JS also verifies a reset after its header
  and groups still being read across the subscription's FIN.
- Remaining: add the count-specific Rust-JS case to track-tail interop. That
  harness currently exposes a relay start-floor defect: when a newer group
  arrives first, earlier in-flight groups can be lost. Finish the count proof
  once that defect is resolved; keep this quest and its PR open until then.

## Related

- [Track tail interop](/quest/m1/track-tail-interop.md) - the Rust-JS case this adds its count check to
- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - makes the count exact by keeping a reset stream's header
