# Migrating moq-srt

How to update your code for each breaking change, newest first.

## Unreleased

### `run` and `Config` are removed

`run`, `Config`, and `Error::ListenerClosed` are gone.
Loop on `Server::accept` and accept or reject each `Request` yourself, as the crate docs show.
`Config::latency` is `Server::bind`'s `latency` argument; `Config::max_age` and `Config::program` are `Publish::with_max_age` and `Publish::with_program`.
To keep the first publisher on a path, track the paths you accepted and reject a second publish to one of them.

### One builder vocabulary

| Before | After |
| --- | --- |
| `Client::with_latency` | `Client::with_max_delay` |

`Client::with_max_age` now takes `impl Into<Option<Duration>>`, so pass a bare `Duration` or `None`.

### A finished ingest lingers

`Publish::accept` and `Client::pull` now resolve only after the ended stream's finished broadcast goes stale.
Drop the future to close the broadcast early. A socket or demux error still closes it at once.

### Egress runs at least one second behind (#5553)

SRT egress delays the TS export by the SRT latency or 1 s, whichever is longer, and the send-ahead now comes out of that budget.
A feed whose video HRD buffer declares a longer delay stops, naming the delay it needs.
