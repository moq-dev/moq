# Migrating moq-gst

How to update your pipelines for each breaking change, newest first.

## Unreleased

### Rust embedders move to gstreamer 0.24 (#6053)

Only Rust code that links `gstmoq` as a library is affected; pipelines and the loadable plugin are not.
`plugin_init`, `plugin_register_static`, and the `glib::Enum` impls on `ConnectionStatus` and `MediaContainer` now use gstreamer 0.24 and glib 0.21 types.
Bump your own `gstreamer` dependency to 0.24 (and `glib` to 0.21) so the `gst::Plugin`, `glib::BoolError`, and `glib::Value` types match.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Set `quic-keylog=true` on `moqsink` or `moqsrc` as well to capture them; without `SSLKEYLOGFILE` the element fails to start.

## 0.3 -> 0.4

### Rate estimates are `estimated-*-rate`

The `moqsink` properties `estimated-send-bitrate` and `estimated-recv-bitrate` are `estimated-send-rate` and `estimated-recv-rate`, with no alias: an application or `gst-launch` line naming the old ones fails at runtime.
