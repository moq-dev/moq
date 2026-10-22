# Migrating moq-rtmp

How to update your code for each breaking change, newest first.

## Unreleased

### A clean end lingers

`Publish::accept` and `Client::pull` now resolve only after a clean end's finished broadcast goes stale: an unpublish (`deleteStream`), or a remote's `NetStream.Play.Complete` or `NetStream.Play.Stop`.
Drop the future to close the broadcast early. A connection that drops without one still closes at once.
`Client::publish` sends `deleteStream` when its broadcast finishes, so the server ingesting it sees a clean end.

### Export staleness is `max_delay`

| Before | After |
| --- | --- |
| `Play::with_max_age` | `Play::with_max_delay` |
| `Client::with_export_max_age` | `Client::with_export_max_delay` |
| `listen::Config::export_max_age` | `listen::Config::export_max_delay` |
| `DEFAULT_MAX_AGE` | `DEFAULT_MAX_DELAY` |

FLV egress now releases each frame on that delay after its decode time, instead of dropping frames older than it.
