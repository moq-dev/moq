# [S] Bindings see a moq-transport run end as an end

## Goal

When a moq-transport publisher ends a run, a moq-ffi consumer (Python, Swift,
Kotlin, Go) sees the same kind of end it sees over moq-lite, not
`MoqError.Internal`. A binding can then follow a replaced publisher over
either protocol by error kind, without treating an IETF run's end as fatal.

## Plan

Decided 2026-10-31 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq.pro#3169](https://github.com/moq-dev/moq.pro/pull/3169)). That
PR's Go and Kotlin samples follow a lite run's end, which reaches the FFI as
a protocol reset (`MoqException.Protocol`), but over moq-transport the old
run's unfinished track becomes `MoqError.Internal` and the samples stop.

The broadcast-closed signal already shipped on main as
`MoqBroadcastConsumer.closed()`
([moq-dev/moq#7304](https://github.com/moq-dev/moq/pull/7304)); it reaches
the bindings with the next release, which moq.pro's reader quest adopts.

Investigation 2026-10-31: stream reset 0 means INTERNAL_ERROR, not a normal
end, on retained drafts [18 section 3.3.3](https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-18#section-3.3.3),
[20 section 3.3.4](https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-20#section-3.3.4),
and [22 section 12.5](https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-22#section-12.5).
Cancellation is 1. PUBLISH_DONE has a separate registry: TRACK_ENDED is 2,
and 0 is INTERNAL_ERROR there too. Incoming 0 must remain an error.

The current FFI publisher already finishes its catalog cleanly on close.
Live-session FFI tests now cover media EOF, catalog EOF, broadcast closure,
and an unfinished track on lite-07 and all retained IETF drafts. The unfinished
track still reproduces the divergence: lite reports Protocol/Unroutable;
IETF's publisher maps Unroutable to PUBLISH_DONE/INTERNAL_ERROR, which arrives
as Remote(0) and becomes MoqError.Internal. Group resets lose the cause too.
No retained draft has an Unroutable code in either registry.

Remaining decision: preserve the wire's errors and use broadcast lifecycle
signals to follow runs (recommended, consistent with announcement-driven
restart), or choose a publisher-side cancellation mapping for route loss in
Rust and JS. Neither TRACK_ENDED nor decoding 0 as EOF is correct for
unfinished content. Do not select a wire mapping without the maintainer's
decision.
