# Migrating moq-rtc

How to update your code for each breaking change, newest first.

## Unreleased

### `Server::bind` binds the media socket

`Server::new(config)` is `Server::bind(addr, config).await?`: it binds the shared UDP socket at `addr` and fails if it cannot, instead of on the first accept.
`server::Socket` and `server::Config::socket` are gone; pass the address you set as `Socket::Bind(addr)`, or `"0.0.0.0:0"` for the old default.
`Server::local_addr` reports the bound address.

### Advertised candidates keep their port

`server::Config::ice_candidates` are advertised as given, port included, instead of with the bound port.
Write the public port you forward to the bound one, or port 0 to keep advertising the bound port.

### `client::Config::ice_candidates` is gone

Nothing read a client's advertised candidates usefully: the server learns a client's address from its ICE checks. Drop the field; `client::Config` has no options yet.

### `Config::max_age` of `None` is unlimited ([#7729](https://github.com/moq-dev/moq/pull/7729))

`server::Config::max_age` and `client::Config::max_age` default to `Some(hang::container::MAX_AGE)` (30 s), and `None` now sets no publisher limit instead of keeping that default.
Leave the field at its default unless you have a value.

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
