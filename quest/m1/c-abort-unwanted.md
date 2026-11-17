# [S] libmoq ends an idle on-demand track without stranding a late reader

## Goal

A C publisher (`rs/moq-c`) that serves a requested track can end it once
nobody wants it, and a reader that joined in between keeps the track instead
of reading the abort or `Dropped`. This covers raw tracks and the media,
encoded audio, and encoded video handles, each of which already exposes a
demand watcher.

## Plan

`moq_net::track::Producer::abort_unwanted` and the typed producers'
`abort_unwanted` (moq-mux media, moq-audio, moq-video, moq-json) exist, and
moq-ffi and every binding over it carry them. libmoq has demand watchers
(`moq_publish_track_demand`, `moq_publish_media_demand`,
`moq_encode_audio_demand`, `moq_encode_video_demand`) but ends a track only by
`moq_publish_track_abort`, a finish, or releasing its handle, so the race
those PRs closed remains here.

Recommended: add `moq_publish_track_abort_unwanted(track, error_code)`,
`moq_publish_media_abort_unwanted`, `moq_encode_audio_abort_unwanted`, and
`moq_encode_video_abort_unwanted`, each returning 1 once the track is closed
(the handle released), 0 when a reader wants it (the handle kept), and a
negative error code as the other calls do; a C fixture per handle where a
reader joins before the call and keeps reading; and the `doc/lib/c` demand
section naming them.
