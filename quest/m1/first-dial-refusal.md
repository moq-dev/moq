# [S] A first session refused at SETUP fails fast

## Goal

A client whose first session is refused right after ALPN (a rejected token on
moq-lite 03 or later, which has no server SETUP to wait for) reports the
refusal and stops, as a first dial that reaches no relay does, instead of
redialing forever. Rust and JS behave the same, and the docs promise only
what the code does.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
`Reconnect` (`rs/moq-net/src/reconnect.rs`) sets `established` as soon as the
transport connects, and `walk_failed` fails fast only while `!established`,
though its comment says that catches "a refused credential". A relay's
`MalformedAuthToken` close then reaches `closed()`, which backs off and redials
forever under `Backoff::timeout: None`.
[moq-dev/moq#8694](https://github.com/moq-dev/moq/pull/8694) hit exactly this
(one refusal a second in the relay log) and changed the interop test rather
than the client. `doc/bin/gstreamer.md` ("only reports `failed` on an answer
redialing can't change, such as a rejected token") and the comments in
`rs/moq-gst/src/sink/session.rs` are false since
[moq-dev/moq#8618](https://github.com/moq-dev/moq/pull/8618).

Mark a session established only once it clears
`Backoff::healthy`, which `Retry::settle` already computes, so no error kind
is read; mirror it in `js/net`'s `#establishedHref`; update `Handover.tla`
through `/verify-reconnect`; fix the gst docs and comments and
`.claude/skills/verify-reconnect/SKILL.md`'s stale refusal exception.
#8618 settled that a refusal of a session that was live retries; this changes
only a first session that never became healthy.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) a session is established only once it clears `Backoff::healthy` (rejected: (b) keep retrying and correct only the docs).

Public API: none. Wire: none. Behavior: a client whose first session is
refused before it is healthy ends with that error.

