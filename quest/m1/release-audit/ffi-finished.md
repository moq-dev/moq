# [M] Keep finished tracks without retaining drained codecs

## Goal

Finishing a binding's audio producer releases its codec and input scratch
storage while preserving the finished track for broadcast completion and late
readers. Repeatedly retiring tracks on one broadcast does not retain every
retired encoder.

## Plan

P2 resource-lifetime concern, source-confirmed but not measured in RSS.
Introduced since release by
[moq-dev/moq#6157](https://github.com/moq-dev/moq/pull/6157). At main 6369eca,
`rs/moq-ffi/src/audio.rs:418-425` drains the producer and passes the whole value
to `Finished::keep`. `producer.rs:113-139` boxes it until the broadcast ends.
The retained `moq_audio::encode::Producer` still owns its encoder and pending
PCM capacity. `finish` only drains the encoder; the libopus backend frees its
native state in `Drop`. Dropping all language-level audio handles cannot
release that retained producer while its broadcast remains live.

Preserving the finished track is intentional. Retain only the publication
ownership needed for that contract, releasing drained codec/scratch state.
Inspect other `Finished::keep` consumers, including video and importers, for
the same excess ownership. Do not claim hardware-encoder exhaustion: the
demonstrated audio backend here is libopus.

Use a drop-observing backend/resource test across multiple finished tracks,
retaining the existing clean-tail and finished-track visibility proofs.
Measure retained resources as retired-track count grows; avoid a brittle RSS
threshold. No public API or wire change is expected. If FFI API shape changes,
update every binding consumer and migration entry together. Current consumers
are long-lived binding publishers that replace or retire audio tracks.
