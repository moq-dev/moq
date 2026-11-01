# [XS] A truncated MKV ends its tracks at finish

## Goal

When the MKV importer's `finish()` refuses truncated input (`Mkv(Truncated)`), it still ends its tracks with what was accepted, as it does after a refused `decode`, so a subscriber's `read()` returns instead of waiting until the importer is dropped. `finish()` still returns the error, and the frame cut short still fails its group with the reason.

## Plan

Found by the verification in [moq-dev/moq#7515](https://github.com/moq-dev/moq/pull/7515), which fed the live `opus.webm` truncated at every length and with every byte flipped: 1462 truncation lengths and 29 byte flips ended with tracks neither finished nor aborted after `finish()` returned `Truncated`, at every chunk size. Callers that drop the importer after an error were unaffected.

In `Import::finish` (`rs/moq-mux/src/container/mkv/import.rs`, about lines 838-852), a failed `drain_eof` aborts only the streaming block's group and returns early, skipping the per-track finish. A `decode` that already failed (`self.failed`) skips `drain_eof` and still reaches the track loop, so it ends its tracks.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m1, a small fix in an importer the cut ships. End the tracks on the truncation path the way a refused `decode` already does (`decode`'s docs: "`finish` ends the tracks with what was accepted"), so the two refusals behave alike. #7515 also floated aborting every track with the truncation error instead; matching the refused `decode` keeps one rule and keeps the groups that did arrive. Test with #7515's harness shape: a truncated `opus.webm` whose subscriber's read ends after `finish()` while the importer is still held.

Public API: none. Wire: none.

## Related

- [Untrusted media parsers have fuzz targets](/quest/m2/media-fuzz.md) - could assert that every track ends after `finish()`, whatever the input
