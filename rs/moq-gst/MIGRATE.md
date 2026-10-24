# Migrating moq-gst

How to update your pipelines for each breaking change, newest first.

## Unreleased

### The `encoder` pad property is gone

`moqsink` no longer measures when buffers arrive, so a pad needs no hint that a local encoder feeds it.
Drop `sink_%u::encoder=true` from pipeline descriptions; every pad now advertises `delay` from the order of its buffer timestamps against the other pads.

### `moqsrc` audio caps are the standard ones

`moqsrc` audio pads now carry `rate` and `channels` as `int`, and Opus pads the mapping fields (`channel-mapping-family`, `stream-count`, `coupled-count`, plus `channel-mapping` beyond stereo) with the OpusHead and an empty OpusTags as `streamheader`, so `decodebin` and `opusdec` accept them and `opusdec` applies the output gain.
Opus pads no longer carry the nonstandard `codec_data` and `stream-format=ogg`; a capsfilter naming either, or a `uint` rate or channel count, must drop it.

### Rust embedders move to gstreamer 0.25 (#6169)

Only Rust code that links `gstmoq` as a library is affected; pipelines and the loadable plugin are not.
`plugin_init`, `plugin_register_static`, and the `glib::Enum` impls on `ConnectionStatus` and `MediaContainer` now use gstreamer 0.25 and glib 0.22 types, which need Rust 1.92.
Bump your own `gstreamer` dependency to 0.25 (and `glib` to 0.22) so the `gst::Plugin`, `glib::BoolError`, and `glib::Value` types match.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Set `quic-keylog=true` on `moqsink` or `moqsrc` as well to capture them; without `SSLKEYLOGFILE` the element fails to start.

## 0.3 -> 0.4

### Rate estimates are `estimated-*-rate`

The `moqsink` properties `estimated-send-bitrate` and `estimated-recv-bitrate` are `estimated-send-rate` and `estimated-recv-rate`, with no alias: an application or `gst-launch` line naming the old ones fails at runtime.
