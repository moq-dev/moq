# [XS] The moq-cli HLS recorder uses the declared target

## Goal

The `moq-cli` HLS recorder sets `EXT-X-TARGETDURATION` from the declared `hls`
duration, as the exporter does since
[moq-dev/moq#7018](https://github.com/moq-dev/moq/pull/7018), so a recording
written live keeps one target instead of rewriting its header as segments
grow.

## Plan

Today `Playlist::push` in `rs/moq-cli/src/hls/record.rs` takes the longest
segment seen so far, rounded up. Follow that PR's overrun rule: list the
segment anyway and warn.

Public API: none.
