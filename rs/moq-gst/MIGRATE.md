# Migrating moq-gst

How to update your pipelines for each breaking change, newest first.

## Unreleased

## 0.3 -> 0.4

### Rate estimates are `estimated-*-rate`

The `moqsink` properties `estimated-send-bitrate` and `estimated-recv-bitrate` are `estimated-send-rate` and `estimated-recv-rate`, with no alias: an application or `gst-launch` line naming the old ones fails at runtime.
