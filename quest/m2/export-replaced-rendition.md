# [M] Container exports outlive a rendition that fails and is replaced

## Goal

`moq export ts`, `fmp4`, `mkv`, and `flv` keep writing when one rendition's
track ends with an error and the catalog lists a replacement, as `moq import
capture` does when it reopens a failed microphone as a new audio track. The
export follows the replacement, or keeps the other media and says so, instead
of ending with the failed track's error.

## Plan

Found while validating the CLI's microphone reopen: with
`moq import capture -- export ts`, the reopen kept the video publishing, yet
the export ended with `audio capture: The requested audio device is not
available` once the old audio track failed
(`rs/moq-mux/src/container/consumer.rs`, "track failed; ending the reader").
An `export h264` of the same broadcast kept writing, since it reads only video.

Before that quest, the failed microphone ended the whole publish, so no
export outlived it; the reopen is what exposes this.

Recommended: a rendition whose track fails while the catalog still lists the
broadcast ends only that rendition's reader, and the export picks up a new
rendition of the same medium the way it handles a catalog change; the export
fails only when no rendition of a selected medium remains. Check what the
catalog-identity republish ([moq-dev/moq#7110](https://github.com/moq-dev/moq/pull/7110))
already does for a cleanly finished track and reuse it.

Public API: none expected. Wire: none.
