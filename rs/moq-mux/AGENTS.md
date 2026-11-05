The transmuxer between containers/codecs and hang broadcasts.

- The keyframe bit drives grouping: only a group's first frame is a keyframe. For audio, mark one frame per intended group, or each frame opens its own QUIC stream. After `cut`, the next write must be a keyframe.
- Frame durations backfill from the next frame in decode order, never across a discontinuity or a backwards gap (B-frames). A given duration is kept.
- Create media tracks with `hang::container::track_info`; a default `moq_net::track::Info` is untimed and refuses timestamped frames.
- An fMP4 fragment's `tfdt` must advance past the previous fragment on its track. ffmpeg's `frag_every_frame` repeats one when interleaving audio and video; use `-frag_duration`.
- Catalog fields that configure the decoder (codec, dimensions, description) are immutable for a rendition; bitrate and similar hints are maximums.
