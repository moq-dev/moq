# Migrating moq-srt

How to update your code for each breaking change, newest first.

## Unreleased

### A finished ingest lingers

`Publish::accept` and `Client::pull` now resolve only after the ended stream's finished broadcast goes stale.
Drop the future to close the broadcast early. A socket or demux error still closes it at once.

### Egress runs at least one second behind (#5553)

SRT egress delays the TS export by the SRT latency or 1 s, whichever is longer, and the send-ahead now comes out of that budget.
A feed whose video HRD buffer declares a longer delay stops, naming the delay it needs.
