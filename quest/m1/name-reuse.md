# [M] Publishers never reuse a finished track's name

## Goal

No first-party publisher finishes a track and then publishes different
content under the same name. Each either aborts the track or numbers its
groups past its predecessor's.

## Plan

moq-transcode already finishes a rung only when its source finished. Others still
finish and re-create: moq-gst's sink on caps renegotiation and `release_pad`
(`rs/moq-gst/src/sink/imp.rs`). Also audit moq-mux's timeline error path,
moq-cli echo retire, moq-rtsp, moq-rtmp, `cpp/obs`, `js/publish`, and the IETF
SUBSCRIPTION_ENDED path in moq-net. Fix each with a test that a restart
never repeats a group sequence.

Public API: none expected. Wire: none.
