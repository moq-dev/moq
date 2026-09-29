# [XS] Documented ffmpeg line flushes audio

## Goal

The documented `ffmpeg ... -f mpegts -pes_payload_size 0 -` publish line stops
holding small audio frames in ffmpeg's muxer for up to 0.35 s. Those clumps
raise the importer's catalog `jitter` for the life of the track, and watch
adds about 230 ms of playout delay to match.

## Plan

Decided: add `-muxdelay 0` everywhere the line appears
(`demo/pub/justfile`, `doc/bin/cli.md`, `doc/concept/standard.md`,
`rs/moq-cli/README.md`). `import ts` needs no PCR lead. The reporter's patch
is ready. Verify by re-measuring the catalog jitter on a quiet-audio source;
there is no automated test for a docs line.

The importer's jitter estimate keeping its maximum forever is a separate
problem; the player side is covered by the audio jitter target.

## Closes

- [#4347](https://github.com/moq-dev/moq/issues/4347) - close this issue when the quest finishes

## Related

- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - a decaying playout estimate softens a transient burst
