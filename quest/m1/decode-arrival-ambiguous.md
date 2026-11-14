# [XS] A decoded picture never reports another access unit's arrival

## Goal

`moq_video`'s decode consumer reports the arrival behind each decoded
picture ([moq-dev/moq#8640](https://github.com/moq-dev/moq/pull/8640)). It
pairs a picture with the first queued arrival at the same timestamp. A
decode error leaves its access unit's arrival queued, so a later access
unit at the same timestamp can report the failed unit's earlier arrival.
A picture whose timestamp matches more than one queued arrival reports
none instead, and a failed unit's arrival is dropped where the decoder
refuses it, if that is cleaner.

## Plan

Decided 2026-11-13 (maintainer, /quest-complete): ✅ merge #8640 as is and
track [CodeRabbit's thread](https://github.com/moq-dev/moq/pull/8640#discussion_r4562953312)
here (rejected: holding #8640 until Frando answers it).

Recommended: CodeRabbit's suggested fix in `rs/moq-video/src/decode/consumer.rs`
`receive`: take the match only when it is the sole queued arrival at that
timestamp. Add a test that feeds a unit the decoder rejects, then a unit at
the same timestamp, and checks the picture's arrival is `None` or the later
unit's. Mirror it in `js/` if the JS decoder reports arrivals by then.

Public API: none. Wire: none.
