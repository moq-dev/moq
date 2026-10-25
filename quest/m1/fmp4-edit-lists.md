# [S] Interop holds the AAC tone to its phase

## Goal

The interop AAC rendition passes `tone.py --phase` like Opus does, now that
the fMP4 importer applies the edit list holding ffmpeg's 1024 AAC priming
samples to the frame timestamps.

## Plan

Drop the `opus` condition from `phase_checked` in `test/interop/interop.sh`,
with the comment's AAC sentence, and the matching sentence in
`test/interop/README.md`. Both sit beside lines
[moq-dev/moq#6647](https://github.com/moq-dev/moq/pull/6647) rewrites, so
land this after it.

With the condition dropped, `just test interop --all --publishers rust`
passes the AAC phase on every phase-checked subscriber except GStreamer,
which reads it 1024 samples early (`+878us`). The cause is its decoder, not
the timestamps: `faad`, which `decodebin` picks there, drops the first
frame's output and stamps the rest one frame early, while ffmpeg's decoder
keeps the priming at the input timestamps (an ffmpeg ADTS encode decodes
with its first audible sample at 2 through `faad` and at 1026 through
ffmpeg; `fdkaacdec` lands later still). Decide whether `moqsrc` compensates
for that decoder delay or the check skips GStreamer's AAC with the reason,
as it skips the native-JS subscriber.

## Related

- [MP4 export](/quest/m2/mp4-export.md) - its open "edit lists" item is the output side
