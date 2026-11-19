# [S] moq-srt's server tests hold under load

## Goal

`moq-srt`'s `server::tests` pass however loaded the machine is, and its test target compiles on its own.

## Plan

Four of them (`a_new_clock_flushes_the_partial_chunk_first`, `a_new_clock_re_anchors_the_pacing`, `a_publisher_marker_keeps_an_srt_receiver_playing`, `a_reorder_is_not_a_rewind`) hit nextest's 60s timeout under a full `just check` on 2026-11-18 at load average 40, on a branch that touched no moq-srt code ([moq-dev/moq#9123](https://github.com/moq-dev/moq/pull/9123)).
Run alone, all fifteen pass in 3.4s.
Reproduce under load first, then fix what waits on wall time.

Run alone, the test target does not compile: `src/server.rs` calls `hang::Catalog::subscribe`, which needs hang's `async` feature, and only workspace feature unification enables it.
Enable it in moq-srt's dev-dependencies, as moq-json does for its own wrappers.

Decided 2026-11-19 (coordinator, maintainer away, merge granted): keep this follow-up.
