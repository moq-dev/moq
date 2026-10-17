# Migrating the obs-moq plugin

How to update your OBS setup for each breaking change, newest first.

## Unreleased

### The video bitrate follows the bandwidth estimate (#5845)

The configured video bitrate is now a ceiling: the encoder drops below it when the uplink narrows, with no opt-out.
An encoder shared with a recording follows too, so give the recording its own encoder to keep it at the full rate.
