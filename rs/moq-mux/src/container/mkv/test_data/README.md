# WebM fixture

`opus.webm` is 0.1 seconds of generated 440 Hz mono audio at 48 kHz,
encoded as six Opus packets in a live WebM Segment with unknown size.
It was generated with ffmpeg 9.0.2:

```sh
ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000 -t 0.1 \
  -c:a libopus -f webm -live 1 -map_metadata -1 -fflags +bitexact opus.webm
```

The importer test checks constructor and streaming error propagation,
then reads all six emitted frames across one-byte and larger chunk boundaries.
