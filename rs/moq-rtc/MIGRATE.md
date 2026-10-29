# Migrating moq-rtc

How to update your code for each breaking change, newest first.

## Unreleased

### `moq_net::Path` has no lifetime

`server::{whip, whep}::accept` take `impl Into<moq_net::Path>`: pass a `Path`, from `moq_net::Path::new(s)?` or `moq_net::Path::from_static`. See moq-net's migration for the rest.

### Client calls run the session

`Client::publish` and `Client::subscribe` resolve once the session ends, instead of once it negotiates.
Spawn or select on the future to run other work alongside it, and drop it to end the session.
A peer disconnect resolves `Ok(())`. A `publish` whose source publisher leaves fails, as does a WHEP server session's `Response::run`.

### One router path

`whip::router(server, publisher)` and `whep::router(server, subscriber)` are private.
Call `server.publish_router(publisher)` and `server.subscribe_router(subscriber)` instead.

### A WHIP `DELETE` lingers

After a `DELETE`, a WHIP server session's `Response::run` finishes the ingest and resolves only once the finished broadcast has stayed up for `server::Config::linger` (default 30 s).
Drop the future to close the broadcast early. A peer that disconnects without one still closes it at once.

### Sessions return their UDP wire source (#5632)

`Client::publish` and `Client::subscribe` return `Result<wire::Source>` instead of `Result<()>`.
Read per-peer byte counts with `source.stats()`, or discard it.
