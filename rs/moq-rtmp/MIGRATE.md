# Migrating moq-rtmp

How to update your code for each breaking change, newest first.

## Unreleased

### `run` and `Config` are removed

`run`, `Config`, `ActivePaths`, and `PathGuard` are gone.
Loop on `Server::accept` and accept or reject each `Request` yourself, as the crate docs show.
Set TLS with `Server::with_tls` and `Server::with_plaintext` instead of `Config::tls` and `Config::plaintext`.
To keep the first publisher on a path, track the paths you accepted and reject a second publish to one of them.

### One builder vocabulary

| Before | After |
| --- | --- |
| `Client::with_import_max_age` | `Client::with_max_age` |
| `Client::with_import_bandwidth` | `Client::with_bandwidth` |
| `DEFAULT_MAX_AGE` | none; the default is 500 ms |

`Publish::with_max_age` now takes `impl Into<Option<Duration>>`, so pass a bare `Duration` or `None`.
The default FLV release delay drops from 2 s to 500 ms, matching the `moq` CLI; call `with_max_delay(Duration::from_secs(2))` to keep the old one.

### A clean end lingers

`Publish::accept` and `Client::pull` now resolve only after a clean end's finished broadcast has stayed up for `with_linger` (default 30 s): an unpublish (`deleteStream`), or a remote's `NetStream.Play.Complete` or `NetStream.Play.Stop`.
Drop the future to close the broadcast early. A connection that drops without one still closes at once.
`Client::publish` sends `deleteStream` when its broadcast finishes, so the server ingesting it sees a clean end.

### Export staleness is `max_delay`

| Before | After |
| --- | --- |
| `Play::with_max_age` | `Play::with_max_delay` |
| `Client::with_export_max_age` | `Client::with_max_delay` |

FLV egress now releases each frame on that delay after its decode time, instead of dropping frames older than it.
