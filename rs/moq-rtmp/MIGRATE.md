# Migrating moq-rtmp

How to update your code for each breaking change, newest first.

## Unreleased

### Export staleness is `max_delay`

| Before | After |
| --- | --- |
| `Play::with_max_age` | `Play::with_max_delay` |
| `Client::with_export_max_age` | `Client::with_export_max_delay` |
| `listen::Config::export_max_age` | `listen::Config::export_max_delay` |
| `DEFAULT_MAX_AGE` | `DEFAULT_MAX_DELAY` |

FLV egress now releases each frame on that delay after its decode time, instead of dropping frames older than it.
