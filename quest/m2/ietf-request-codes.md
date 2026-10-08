# [M] moq-transport requests get the draft's answer

## Goal

On draft-18 and draft-21, `rs/moq-net` and `js/net` answer these requests
the way the drafts say, without closing the session. Each answer uses the
draft's code, or an existing code where the exact one costs more than it
is worth and the fallback is recorded in `doc/concept/standard.md`:

- **Range Filters** get REQUEST_ERROR INVALID_FILTER (0x36), not
  NOT_SUPPORTED (draft-19 and later). We advertise no MAX_FILTER_RANGES, so
  the default of 0 applies (draft-21 §9.1.6) and every Range Filter is over
  the limit. This covers SUBSCRIBE and FETCH alike: both refuse Range
  Filters today (`run_subscribe_stream` and `run_fetch_stream` in
  `ietf/publisher.rs`).
- **Draft-21 reserved namespaces:** draft-21 §2.4.2 and the `.session`
  rules (§6.5) ask for REQUEST_ERROR DOES_NOT_EXIST, without passing the
  request to the application, for:
  - a request for an unrecognized `.session` track or namespace;
  - a `.session` namespace with an empty track name;
  - a namespace whose first field is exactly `.`.

  Only these short-circuit. Any other reserved namespace (a first field
  starting with `.`) still goes to the application, as §2.4.2 requires.
- **RENDEZVOUS_TIMEOUT:** our wait is capped at 0, which draft-21 §9.20.7
  allows. If a SUBSCRIBE asks to wait and there is no publisher, it gets
  TIMEOUT at once. A request that is absent or 0 still gets DOES_NOT_EXIST.
  This fixes the two quic-zig `rendezvous-timeout` cells in the interop
  matrix.
- **OBJECT_DELIVERY_TIMEOUT (0x02) and SUBGROUP_DELIVERY_TIMEOUT (0x06)**
  on REQUEST_UPDATE are accepted and ignored, as SUBSCRIBE already does
  (`ietf/subscribe.rs`). Today `ietf/request_stream.rs` decodes both and
  marks the update unsupported.

## Plan

Decided with the maintainer on 2026-10-04 and 2026-10-05:

- **Deliberate deviations stay.** DUPLICATE_SUBSCRIPTION and PREFIX_OVERLAP
  are never sent, because moq-net deduplicates. OBJECT_DELIVERY_TIMEOUT and
  SUBGROUP_DELIVERY_TIMEOUT never drop an object or reset a subgroup: they
  would need wall-clock delivery deadlines, and moq-net's latency
  enforcement is presentation time only (#2890). The rendezvous wait is
  capped at 0. Each deviation gets a line under
  "moq-transport" in `doc/concept/standard.md`, so the next validator run
  has a reason to point at.
- **Exact codes when cheap.** INVALID_FILTER becomes a request code. Where
  one costs more than it is worth, the fallback in the Goal applies.
- **TIMEOUT, not a timer.** The 0 cap means nothing waits, so no request is
  ever parked. The reply depends only on whether the parameter asked for a
  wait. `ietf/subscribe.rs` parses RENDEZVOUS_TIMEOUT and discards it;
  keep it as far as the publisher's track lookup (`run_subscribe_stream`
  in `ietf/publisher.rs`). `Error::Timeout` already maps to
  TIMEOUT (`ietf/error.rs`).
- Decided 2026-10-08: start after [Parameters per
  draft](/quest/m0/ietf-params-per-draft.md)
  ([#5028](https://github.com/moq-dev/moq/pull/5028)), which rewrites the
  same parameter decode and leaves the REQUEST_UPDATE delivery timeouts to
  this quest.
- One regression test per case, failing without its fix and asserting the
  draft's code, or the fallback recorded in `doc/concept/standard.md`.
  Mirror in `js/net/src/ietf` in the same PR.

Public API: none expected. Wire: none new; replies move closer to the
drafts.

## Required

- [Parameters per draft](/quest/m0/ietf-params-per-draft.md) - rewrites the parameter decode this builds on

## Related

- [Malformed moq-transport input](/quest/m2/ietf-malformed-close.md) - the session-level half of the same validator report
