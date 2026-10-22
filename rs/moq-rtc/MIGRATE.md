# Migrating moq-rtc

How to update your code for each breaking change, newest first.

## Unreleased

### Sender-report times must fit the portable wall range

A str0m sender report outside 1970-2554 now fails the ingest session with `Error::Rtc`, instead of admitting an unrepresentable MoQ wall time.
The conversion occurs at the str0m boundary; the public Rust API and RTP encoding are unchanged.

### Sessions return their UDP wire source (#5632)

`Client::publish` and `Client::subscribe` return `Result<wire::Source>` instead of `Result<()>`.
Read per-peer byte counts with `source.stats()`, or discard it.
