# Migrating the obs-moq plugin

How to update your OBS setup for each breaking change, newest first.

## Unreleased

Connection state and first-dial failures now arrive through one lifecycle watcher. UI reads no longer poll the Client or Session for failure separately.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Turn on **TLS key log** in the output's advanced settings or the MoQ Source's properties as well to capture them; without `SSLKEYLOGFILE` the stream stops or the source does not connect.

### The video bitrate follows the bandwidth estimate (#5845)

The configured video bitrate is now a ceiling: the encoder drops below it when the uplink narrows, with no opt-out.
An encoder shared with a recording follows too, so give the recording its own encoder to keep it at the full rate.
