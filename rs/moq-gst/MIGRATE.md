# Migrating moq-gst

How to update your pipelines for each breaking change, newest first.

## Unreleased

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Set `quic-keylog=true` on `moqsink` or `moqsrc` as well to capture them; without `SSLKEYLOGFILE` the element fails to start.

## 0.3 -> 0.4

### Rate estimates are `estimated-*-rate`

The `moqsink` properties `estimated-send-bitrate` and `estimated-recv-bitrate` are `estimated-send-rate` and `estimated-recv-rate`, with no alias: an application or `gst-launch` line naming the old ones fails at runtime.
