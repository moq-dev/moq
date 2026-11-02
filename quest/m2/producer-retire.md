# [S] Replaced producers retire their records

## Goal

An application that keeps replacing a rendition's producer on one broadcast
does not keep one HLS record track per replacement until the broadcast ends.
moq-video, moq-audio, moq-ffi, and moq-c retire a replaced producer's
records, as the container importers and `moqsink` do since
[moq-dev/moq#6937](https://github.com/moq-dev/moq/pull/6937).

## Plan

Decided 2026-10-28 (maintainer): m2, since the churn comes from the local
application, not a network peer.

- Each of these mints a fresh track name per producer. Call
  `moq_mux::catalog::Producer::retire` when a producer is replaced or
  dropped mid-broadcast, after its last frame is flushed and its recorder is
  released (`retire` errors while the recorder is alive).
- Keep #6783's rule that a dropped rendition's name can return with its
  record numbering continued: only the owner that minted a name retires it.
- Test repeated replacement in each: record tracks stay flat.

Public API: none expected; the bindings may document that replacing a
producer ends its records. Wire: none.

## Related

- [Re-init retires timelines](/quest/m2/reinit-retire.md) - the same bound for fMP4 and MKV re-inits
