# [S] The Rust audio subscriber closes within its drain

## Goal

`just test interop --all` passes its `* -> rust` cells under load: the
`interop-audio` client (`test/interop/clients/rust/audio.rs`) closes its
session without `Session::close` returning `Error::Timeout`.

## Plan

Seen once on 2026-11-19 while validating
[moq-dev/moq#9155](https://github.com/moq-dev/moq/pull/9155): the
`python -> rust` cell decoded the whole tone (`decoded 28848 samples of tone
(opus) with libopus`), then failed with `Error: timeout` after 7 s of its 30 s
budget. The client had connected over the WebSocket fallback, a sign the host
was loaded. Three reruns of the cell alone passed, and main's scheduled
Interop runs show no such cell. The output does not say which deadline
returned the timeout. The likeliest is the subscriber's close drain (one second,
`rs/moq-net/src/session.rs` `poll_drain`), but `Error::Timeout` also comes
from the route stream (`rs/moq-net/src/lite/subscriber/route_stream.rs`) and
the publisher and server deadlines. First find which one fired, by logging the
error's path in the client after it decodes, then what it still waited on, and
fix that cause rather than the deadline or the client's `?`.

Public API: none. Wire: none.
