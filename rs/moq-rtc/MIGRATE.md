# Migrating moq-rtc

How to update your code for each breaking change, newest first.

## Unreleased

### One router path

`whip::router(server, publisher)` and `whep::router(server, subscriber)` are private.
Call `server.publish_router(publisher)` and `server.subscribe_router(subscriber)` instead.

### A WHIP `DELETE` lingers

After a `DELETE`, a WHIP server session's `Response::run` finishes the ingest and resolves only once the finished broadcast goes stale.
Drop the future to close the broadcast early. A peer that disconnects without one still closes it at once.

### Sessions return their UDP wire source (#5632)

`Client::publish` and `Client::subscribe` return `Result<wire::Source>` instead of `Result<()>`.
Read per-peer byte counts with `source.stats()`, or discard it.
