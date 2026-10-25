# [M] moqsrc owns each rendition pad in one actor

## Goal

moqsrc's per-rendition pad lifecycle (stream-start, caps, segment, EOS,
retire) is owned by one actor per pad driven by a single command stream, so
restarts and reconnects stop racing over shared pad state.

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. Internal only; no bug was
found in the current state.

Repeated fixes land on pad ownership across restarts and reconnects in
`rs/moq-gst/src/source/imp.rs`: e83f490a44, 83ae631068, 1b5fba232a,
6559065b8c, 8ba01b91f4, a0929c90b3, b9e91f2c65, abf3fc43f7, plus three
test-race fixes. The state is spread across `Pads`/`Slot` (an owner
`tokio::Mutex` plus `retiring`/`listed`), `Finished`, `Follower`, the
`ReaderHandle` `live` flag, and per-reader cancel watches.

One actor per rendition pad owns the `gst::Pad` and its events and takes
play, idle, and retire commands from one stream. Keep the existing moqsrc
tests as the guard, and add a restart-under-reconnect test that interleaves
commands.

Public API: none. Wire: none.
