# [L] moq-video has a producer that takes frames

## Goal

`moq_video::encode::Producer` takes raw frames, owns its encoder, and follows
the uplink grant, as `moq_audio::encode::Producer` does for PCM. The type
that publishes already encoded access units keeps working under a name that
says what it takes (recommended: `encode::Publisher`). moq-boy, moq-ffi, and
moq-c publish video through the new producer, so the probe, catalog
reservation, encode loop, and grant following exist once, in moq-video.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Rename today's type and build the frame-taking producer before the cut
  (rejected: rename only and add the producer after the cut under the freed
  name; keep both names and add the frame-taking type later under a third).
  The maintainer: "Rename and build it now." A name that means two layers in
  sibling crates freezes at the cut, and the freed name is only worth
  reserving if the type that earns it ships with it.

Facts found by the audit:

- `moq_audio::encode::Producer` takes PCM (`write`,
  `rs/moq-audio/src/encode/producer.rs:407`), owns its encoder, and follows
  the grant through `Options::bandwidth` (`:43`).
  `moq_video::encode::Producer` takes encoded access units (`publish`,
  `rs/moq-video/src/encode/producer.rs:163`) and knows no encoder or grant.
  Same name, same module path, different layer.
- Every caller with its own frames assembles the video path by hand: probe
  the config, reserve a catalog slot with the effective join, build the
  producer, run the encode, receive, publish loop. `rs/moq-boy/src/video.rs:59-65`
  is the shortest witness; `rs/moq-ffi/src/video.rs` (2,129 lines) and
  `rs/libmoq/src/video.rs` (1,820 lines) carry the rest.
- Grant following with rung disable, the part that is easy to get wrong,
  exists three times: private in the capture driver
  (`rs/moq-video/src/encode/producer.rs:718`, `:840`, `:940`), in moq-ffi
  (`rs/moq-ffi/src/video.rs:370`, `:524`), and in moq-c
  (`rs/libmoq/src/video.rs:286`, `:319`, `:363`).
- No quest owns it: [the grant getter](/quest/m3/video-publish-grant.md)
  only adds a getter to the capture handle, and
  [the mux wrap](/quest/m5/net-wrap/mux.md) only makes the producer generic.

For the PR:

- Rename first, as its own commit: mechanical, about 19 references outside
  moq-audio (`grep -rn 'encode::Producer' rs`). The rename also reaches the
  docs (`doc/lib/rs/moq-video.md`) and `rs/moq-video/README.md`.
- Lift the frame-taking producer out of the capture driver
  (`publish_capture`'s `Driver`), which then runs on it, so the capture path
  and a caller's own frames share one encode loop and one grant follower.
  Shape it after audio: an `Options` with `bandwidth`, `write(&Frame)`, and
  `finish(self) -> Result<Finished, Error>` from the start (the
  [finish sweep](/quest/m2/finish-sweep.md) converts the renamed type).
- Move moq-boy, moq-ffi, and moq-c onto it and delete their copies of the
  grant follower. A binding option the lifted type cannot express (moq-ffi's
  `lower` rendition for the simulcast ladder, `rs/moq-ffi/src/video.rs:370`)
  becomes an option of the producer, not a reason to keep a copy.
- If lifting the driver would change a backend's behaviour in a way the cut
  cannot absorb, stop and say so on the PR instead of shipping the rename
  alone: the maintainer rejected that end state.
- Tests: the capture driver's grant tests run against the lifted type; a
  frame-taking producer follows a falling and recovering grant and disables
  and re-enables its rendition; `just test interop --all` for moq-ffi.

Public API: moq-video breaks (`encode::Producer` changes meaning; the encoded
publisher is renamed). moq-ffi and moq-c keep their names. Wire: none.

## Related

- [Terminal operations consume](/quest/m2/finish-sweep.md) - gives the renamed type `finish(self)`; whichever lands second rebases
- [One device selector](/quest/m2/native-media-config.md) - changes `encode::Config` in the same crate
- [A published capture exposes its grant](/quest/m3/video-publish-grant.md) - adds a getter to the capture handle this runs on
