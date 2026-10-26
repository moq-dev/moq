# [M] Audio mixes

## Goal

The program bus in moq-audio feeds several mixes. For example, a stream mix
excludes the creator's music, a recording mix keeps it, and a monitor mix
plays to headphones. Each source routes to any subset of mixes, and each
output or recording picks a mix. Every source and mix shows a level meter.
Per-source sync offsets fix cameras that lag their microphones, and a clap
or flash in front of the camera measures the offset instead of guessing it.
Each guest hears a mix-minus: the program without their own voice.

## Plan

The mixer runs on the audio clock and never blocks on a slow output. An
output that falls behind drops its own audio, not the bus's.

Benchmark source count against mix count, including sparse routing. A mix
must cost only what is routed into it, never a scan of every source.
