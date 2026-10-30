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
  moqsink name=mux url=https://cdn.moq.dev/anon broadcast=<your-name>.hang
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
Opus buffers are read the way `opusenc` and `moqsrc` stamp them: at the time
their first kept sample plays, with `GstAudioClippingMeta` clipping the
pre-skip. Each is published the pre-skip, less its own clipping, later, since
a frame timestamp counts the pre-skip.

A `text/x-raw` pad is how captions get in: ffmpeg cannot mux a subtitle track
into fragmented MP4, so `moq import fmp4` can't carry one, while a demuxer that
resolves timed text (`qtdemux` on a 3GPP timed-text track) can feed the pad
directly. A cue with no duration is dropped rather than left on screen.

Audio packets share groups of at least 20 ms while forwarding as they arrive.
Each `sink_%u` request pad is one track. Pad properties: `track` names it
(default: after the codec), `container=loc` publishes it as
[LOC](/concept/standard#loc) instead of the legacy hang container, and
`track-status`/`track-error` report its lifecycle. Caps that only change what
the bitstream already carries (a video resolution) keep the track live. A run
never publishes a track name it ended again: new codec config, a pad requested
again, or a stream resumed after EOS publishes as `<track>/<n>`, which `track`
reads back. Element properties:
`url`, `broadcast`, `linger`, `connect-version`, `tls-disable-verify`, `quic-idle-timeout`, `quic-keylog`,
`quic-keep-alive`, and read-only `status`, `connected`, `moq-version`, and
`estimated-send-rate`, `estimated-recv-rate`, `connection-stats`, and
`sessions`. The sink reconnects for as long as the pipeline runs and only
reports `failed` on an answer redialing can't change, such as a rejected token.
Both elements accept the same connection properties. `connect-version` is a
comma-separated list, matching the CLI's repeatable `--connect-version`;
unknown versions fail startup. QUIC durations are milliseconds, with zero
disabling the local idle timeout or keep-alive.
Each run from `READY` publishes under a fresh
[publisher epoch](/concept/moq-lite#publisher-epochs), so on moq-lite 07 (opt-in)
or moq-transport, viewers switch to a restarted pipeline at once instead of stalling on the old
one; reconnects within a run keep it. The first catalog waits for a timed buffer
to place the shared wall clock and for every requested pad to negotiate its
rendition, including any codec config carried in its first buffers. A linked pad
that never negotiates holds back the whole catalog until
it is released, reaches EOS, or fails; a flush or new stream after that EOS
holds it back again. Opaque data pads are not listed. Later
catalog updates do not wait for newly requested pads.

Once every pad reaches EOS, the sink keeps the finished broadcast announced
for `linger` milliseconds (default 30000), so a subscriber that arrives late
still gets the tail, then closes it and posts EOS. `linger=0` closes it at once,
and stopping the pipeline (`READY` or `NULL`) during the wait closes it early.

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

The catalog `delay` of each pad is how far its buffers' timestamps trail ones
the other pads already pushed, so players buffer for an encoder that delivers
behind the others. Arrival time is never measured, so file, demuxed, and network
media advertise the same values as a live encoder.

After a pause, flush, or changed TIME segment, the pad stops holding the others
back until its next buffer, and previously measured maxima remain. Resumed
timestamps must continue forward on the broadcast media clock.

A video pad joining mid-GOP drops delta frames until its first keyframe. If a
source rewinds below the producer's live edge without signalling a break, the
pad drops that frame and waits for a keyframe at or beyond the live edge. It
keeps the rendition alive and preserves the media timeline; it does not shift
rewound timestamps forward.

## moqsrc

Pads are named by kind and appear as the catalog announces renditions:
`video_0`, `video_1`, `audio_0`. A rendition referencing another broadcast reads
that broadcast on the same connection and follows its announcements the same way. Link the pad you want by name; the terse
`moqsrc ! decodebin3` form links only the first pad offered, which may be
audio. `moqsrc` follows the broadcast's announcements: when the publisher
restarts, it switches to the new broadcast on the same pads, kept by rendition
name, so a pipeline linked by name keeps playing. A format change keeps its pad
too, with new caps downstream must accept. The new run's first catalog must list
every returning rendition; one it omits gets EOS instead of keeping its old pad.
Within each run, the first media frame fixes one timestamp reference and segment
base for every pad. Audio and video keep their relative broadcast timing, even
when their tracks use different timescales or arrive at different times. Frames
before that reference keep an earlier PTS for downstream to clip. Opus packets
covering the OpusHead pre-skip carry `GstAudioClippingMeta` for it, and each
Opus packet that keeps samples is stamped where the first one plays, so `opusdec`
drops the priming samples and the audio lines up with video. A new broadcast
chooses a fresh reference on the same pads; playback does not require a wall clock.
A rendition that leaves the catalog
gets EOS once its track ends. A broadcast that ends or loses its publisher holds
its pads without EOS until it is announced again, so the pipeline does not end
on its own. Losing the relay connection also holds the pads: `moqsrc` redials
without a retry deadline and resumes when the broadcast returns. A refused
connection, refused path or catalog, or malformed protocol input or catalog posts
an error. Properties: `url`,
`broadcast`, `connect-version` (comma-separated protocol versions to offer, most
preferred first; every stable version by default), `tls-disable-verify`,
`quic-idle-timeout`, `quic-keep-alive`, and `quic-keylog`.

Debug with `GST_DEBUG=*:4` for GStreamer and `RUST_LOG=debug` for the plugin.
To decrypt a capture, set `quic-keylog=true` on either element along with
`SSLKEYLOGFILE`; the variable alone writes nothing, and the flag without it fails
the start. Existing Unix log files must be private (mode 0600); files accessible to group or others are refused.

`moqsrc` initially selects only enabled renditions. When a selected rendition is
disabled, it keeps reading on the same pad while no rendition of its kind is
enabled, so media arriving before the re-enable catalog update still plays. An
enabled alternative stops the disabled reader but keeps its pad idle without EOS;
enabling it again resumes on that pad. Removing a rendition drains its track and
ends its pad.
