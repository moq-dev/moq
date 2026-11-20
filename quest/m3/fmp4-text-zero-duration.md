# [S] fMP4 text import accepts ffmpeg's live TTML

## Goal

`moq import fmp4` publishes the cues of ffmpeg's fragmented TTML output
(`-c:s ttml -f mp4 -movflags cmaf+separate_moof+delay_moov+skip_trailer -frag_duration 1000`)
with the timing the source documents give, instead of ending the import.
A text sample whose duration truly cannot be recovered fails with an error
that says so, not one about multi-sample fragments.

## Plan

Found 2026-11-18 while answering
[moq-dev/moq#7945](https://github.com/moq-dev/moq/issues/7945#issuecomment-6975233746).
ffmpeg 9.0.2 is the tool most publishers reach for, and it cannot write
WebVTT into MP4, so TTML is its only subtitle path into `import fmp4`.

Reproduce: encode `testsrc2`, a `sine`, and any `.vtt` file with
`-c:s ttml` and the flags above, then pipe or redirect into
`moq import fmp4`. It ends with `cmaf: multi-sample fragment has a non-final
sample with no duration; DTS is unrecoverable`.

What the file holds (MP4Box `-diso`): every text fragment is a single-sample
`traf` whose `tfhd` sets `default_sample_duration`, most often to 0.
`Defaults::resolve` (`rs/moq-mux/src/container/fmp4/sample.rs`) reads 0 as
unknown, which is right for media, and the text path
(`rs/moq-mux/src/container/fmp4/text.rs`, `sample.duration.ok_or(...)`)
refuses any text sample without one, even a final sample, reusing the
multi-sample error. ffprobe also shows ffmpeg repeating a document at the
same presentation time across fragments (several 607-byte samples at
0.733 s); find out what those repeats mean before implementing, and bring
it back to the maintainer if they undercut the fix below.

Decided 2026-11-18 (maintainer):

- ✅ Fix it, in m3 (rejected: won't fix and document GPAC as the supported
  subtitle muxer, which leaves ffmpeg users with a hard error).
- ✅ End a zero-duration text sample at the next fragment's `tfdt` on that
  track, as media already recovers a final sample's duration, holding one
  text sample back, and give the unrecoverable case its own error.

Check cues come out on the catalog clock with their source spans by fetching
the text track during a live publish (`moq fetch --json --group <µs> <track>`).

Caused by: [moq-dev/moq#5281](https://github.com/moq-dev/moq/pull/5281)
added the fMP4 text path with `sample.duration.ok_or(MissingSampleDuration)`,
refusing a final text sample with no duration; TTML import
([moq-dev/moq#5855](https://github.com/moq-dev/moq/pull/5855)) inherited it.

Public API: none. Wire: none.

## Related

- [Audio renditions carry a language](/quest/m3/audio-lang.md) - its CLI demo uses GPAC's WebVTT until this lands
