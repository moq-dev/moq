---
title: GStreamer Plugin
description: moqsink and moqsrc elements
---

# GStreamer Plugin

Two elements: **moqsink** publishes a pipeline to a relay, **moqsrc**
subscribes to a broadcast and exposes one source pad per rendition.

```bash
# Inspect (Nix bundles the plugin with gst-launch)
nix shell github:moq-dev/moq/release#moq-gst --command gst-inspect-1.0 moq

# Play the public test broadcast
nix shell github:moq-dev/moq/release#moq-gst --command gst-launch-1.0 -e \
  moqsrc name=s url=https://cdn.moq.dev/demo broadcast=bbb.hang \
  s.video_0 ! queue ! decodebin3 ! videoconvert ! autovideosink \
  s.audio_0 ! queue ! decodebin3 ! audioconvert ! autoaudiosink

# Publish a test pattern
gst-launch-1.0 -e videotestsrc is-live=true ! x264enc tune=zerolatency ! h264parse \
  ! video/x-h264,stream-format=byte-stream,alignment=au ! mux.sink_0 \
  moqsink name=mux url=https://cdn.moq.dev/anon broadcast=<your-name>.hang sink_0::encoder=true
```

Install via `apt install gstreamer1.0-moq` or `dnf install gstreamer1-moq`
([Install](/setup/install)). With Homebrew, run `brew install moq-dev/tap/moq-gst`
and add `$(brew --prefix moq-gst)/lib/gstreamer-1.0` to `GST_PLUGIN_PATH_1_0`.
For a source build, run `cargo build -p moq-gst` and point
`GST_PLUGIN_PATH_1_0` at the output. `http://` URLs pin the relay's
certificate fingerprint automatically, so local development needs no TLS setup.
That scheme is for localhost only: the fingerprint is fetched unauthenticated
and the WebSocket fallback runs as cleartext `ws://`, so use `https://` for
anything else.

## moqsink

| Codec | Caps |
| --- | --- |
| H.264, H.265, AV1, VP8, VP9 | `video/x-h264`, `video/x-h265`, `video/x-av1`, `video/x-vp8`, `video/x-vp9` |
| AAC, MP3, Opus | `audio/mpeg`, `audio/x-opus` |
| Captions | `text/x-raw` (one WebVTT cue per buffer, PTS is the cue start and the buffer duration its end) |
| Opaque data | `application/octet-stream` (raw bytes on a named track, one group per buffer) |

`audio/x-opus` is published from the caps' OpusHead. A `streamheader` is that
head, including pre-skip and gain, and it wins when present. Otherwise mono
and stereo use a family 0 head, and three to eight channels need
`channel-mapping-family`, `stream-count`, `coupled-count`, and
`channel-mapping`. Caps that omit that mapping, or that contradict the header,
are refused. `opusenc` supplies both, so a 5.1 encode plays as six channels.

A `text/x-raw` pad is how captions get in: ffmpeg cannot mux a subtitle track
into fragmented MP4, so `moq import fmp4` can't carry one, while a demuxer that
resolves timed text (`qtdemux` on a 3GPP timed-text track) can feed the pad
directly. A cue with no duration is dropped rather than left on screen.

Audio packets share groups of at least 20 ms while forwarding as they arrive.
Each `sink_%u` request pad is one track. Pad properties: `track` names it
(default: after the codec), `container=loc` publishes it as
[LOC](/concept/standard#loc) instead of the legacy hang container,
`encoder=true` marks it as fed by a local encoder, and
`track-status`/`track-error` report its lifecycle. Element properties:
`url`, `broadcast`, `tls-disable-verify`, `quic-idle-timeout`,
`quic-keep-alive`, and read-only `status`, `connected`, `moq-version`, and
`estimated-send-rate`, `estimated-recv-rate`, `connection-stats`, and
`sessions`. The sink reconnects for as long as the pipeline runs and only
reports `failed` on an answer redialing can't change, such as a rejected token.
Each run from `READY` publishes under a fresh
[publisher epoch](/concept/moq-lite#publisher-epochs), so on moq-lite 07 (opt-in)
or moq-transport draft 17 and later, viewers switch to a restarted pipeline at once instead of stalling on the old
one; reconnects within a run keep it. The first catalog waits for a timed buffer
to place the shared wall clock and for every requested pad to negotiate its
rendition, including any codec config carried in its first buffers. A linked pad
that never negotiates holds back the whole catalog until
it is released, reaches EOS, or fails; a flush or new stream after that EOS
holds it back again. Opaque data pads are not listed. Later
catalog updates do not wait for newly requested pads.

All pads share one media timeline on one wall clock. The first timed buffer places
it using UNIX or NTP reference timestamp metadata; other clock domains use the
pipeline running time and local wall clock. Delayed buffers keep their media
time. Pauses and flushes keep this mapping; starting a new run chooses a new one.

`connection-stats` is null while disconnected. While connected it is a
structure of the transport counters the active backend has (RTT, send and
receive estimates, bytes, packets, loss). Missing ones are omitted rather than
reported as zero. Poll it for the current values; property notification marks
connect and disconnect.

`sessions` is the cumulative connect and disconnect counts for this element,
in the same shape as the relay's sessions track. One read returns both from
the same instant, so their difference is 1 while connected. Unlike `status`, a
drop you did not poll still moves both counters.

Set `encoder=true` on audio and video pads a local encoder feeds
(`x264enc`, `opusenc`, ...). The pad then measures how late each frame reaches
the sink behind its running time and raises the catalog `jitter` by the spread,
and `delay` by how far it trails the earliest such pad, so players buffer for an
encoder that delivers irregularly or behind the others. Leave it off, the
default, for file, demuxed, and network media: their arrival reflects the disk
or the network, not the original encoder, and a GStreamer segment cannot tell
the two apart. Text and opaque pads refuse it.

After a pause, flush, or changed TIME segment, the next media buffer resets
the handoff measurement baseline. The pause does not inflate
advertised jitter, and previously measured maxima remain. Resumed timestamps
must continue forward on the broadcast media clock.

A video pad joining mid-GOP drops delta frames until its first keyframe. If a
source rewinds below the producer's live edge without signalling a break, the
pad drops that frame and waits for a keyframe at or beyond the live edge. It
keeps the rendition alive and preserves the media timeline; it does not shift
rewound timestamps forward.

## moqsrc

Pads are named by kind and appear as the catalog announces renditions:
`video_0`, `video_1`, `audio_0`. A rendition referencing another broadcast reads
that broadcast on the same connection. Link the pad you want by name; the terse
`moqsrc ! decodebin3` form links only the first pad offered, which may be
audio. `moqsrc` follows the broadcast's announcements: when the publisher
restarts, it switches to the new broadcast on the same pads, kept by rendition
name, so a pipeline linked by name keeps playing. A format change keeps its pad
too, with new caps downstream must accept. The new run's first catalog must list
every returning rendition; one it omits gets EOS instead of keeping its old pad.
Within each run, the first media frame fixes one timestamp reference and segment
base for every pad. Audio and video keep their relative broadcast timing, even
when their tracks use different timescales or arrive at different times. Frames
before that reference keep an earlier PTS for downstream to clip. A new broadcast
chooses a fresh reference on the same pads; playback does not require a wall clock.
A rendition that leaves the catalog
gets EOS once its track ends. A broadcast that ends or loses its publisher holds
its pads without EOS until it is announced again, so the pipeline does not end
on its own. Losing the relay connection also holds the pads: `moqsrc` redials
without a retry deadline and resumes when the broadcast returns. A refused
connection, refused path or catalog, or malformed protocol input or catalog posts
an error. Properties: `url`,
`broadcast`, `tls-disable-verify`.

Debug with `GST_DEBUG=*:4` for GStreamer and `RUST_LOG=debug` for the plugin.

`moqsrc` creates source pads only for enabled renditions. Disabling a rendition stops its media and holds its pad idle without EOS; enabling it again resumes on the same pad.
