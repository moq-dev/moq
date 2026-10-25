# Migrating the obs-moq plugin

How to update your OBS setup for each breaking change, newest first.

## Unreleased

Connection state and first-dial failures now arrive through one lifecycle watcher. UI reads no longer poll the Client or Session for failure separately.

### The audio-only and video-only outputs are gone (#6762)

The plugin registers only `moq_output`; `moq_output_audio` and `moq_output_video` no longer exist.
If an obs-websocket request or script creates either by id, create `moq_output` instead, and attach both a video and an audio encoder: `moq_output` does not start without both, so video-only streaming is no longer possible.
The audio-only output never worked: it failed to start without a video encoder.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Turn on **TLS key log** in the output's advanced settings or the MoQ Source's properties as well to capture them; without `SSLKEYLOGFILE` the stream stops or the source does not connect.

### The video bitrate follows the bandwidth estimate (#5845)

The configured video bitrate is now a ceiling: the encoder drops below it when the uplink narrows, with no opt-out.
An encoder shared with a recording follows too, so give the recording its own encoder to keep it at the full rate.
