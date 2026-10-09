# [S] A flate stream refuses an oversized append without ending

## Goal

`moq-flate` and `@moq/flate` `stream` mode refuse an append that cannot fit
the group budget with `GroupTooLarge` and leave the log intact, compressed or
not, as JSON streams do after #4911. In deflate mode, a payload beyond the
decoder's frame size limit is also refused before encoding without ending the
log.

## Plan

Flate's stream mode rides one group, like JSON's, so it has the same hole.
Move the DEFLATE worst-case bound (`deflateBound` / `deflate_bound`) from json
into flate and have json reuse it, so the bound and its tests live in one
place per language. Rust's helper is private; JS's is exported from its
encoder module, so expose only what json needs from flate.

The decoder-size check only runs in deflate mode (`self.flate.is_some()` in
`rs/moq-flate/src/stream/producer.rs`, `this.#flate &&` in
`js/flate/src/stream/producer.ts`). Refusing it without ending the track
deliberately reverses both producers' current behavior and their comments,
which treat it as terminal like any other lost record; update those comments.

Check both limits before mutating the compression window. A refusal leaves
prior records readable and allows a later fitting append; a failed write
after encoding still aborts, as JSON does. Mirror #4911's tests in both
compression modes, including the decoder-size refusal, and document the
budget and refusal behavior in the flate library docs.
