# [L] Publishers pad to find room for a stopped rendition

## Goal

A publisher that stopped a simulcast rendition for lack of bandwidth sends
short, low-priority padding bursts at that rendition's resume rate, so the
congestion controller can measure room above the media it still sends. When
the uplink recovers, the bandwidth estimate rises and the rendition resumes.
When the uplink doesn't recover, the padding stays within its budget and never
delays media. This holds for native publishers (moq-net over any QUIC backend)
and browser publishers (`@moq/publish` over WebTransport).

## Plan

The estimators behind the grant stop learning once a stream is app-limited.
moq-quic's BBR3 reports `BBR.max_bw`, which an app-limited sample can only raise
past its current value, and its window only ages on a ProbeBW cycle that isn't
app-limited (`update_max_bw` and `adapt_long_term_model` in
`rs/moq-quic/src/congestion/bbr3/mod.rs`). Chromium's QUICHE gates
`max_bandwidth_filter_` the same way in `bbr2_misc.cc`. Its Cubic estimate is
cwnd / srtt, and the window doesn't grow while the sender isn't cwnd-limited
(`tcp_cubic_sender_bytes.cc`). So after the top rendition stops, the estimate
freezes at the value that stopped it, and nothing sent today makes it rise:
moq-net advertises PROBE `Report` only.

Decided (2026-10-17, quest-plan; the user chose padding over timed resumes
and over landing without probing):

- **Padding at the application layer, not in the QUIC stack.** A browser
  can't reach its congestion controller, and native publishers run on several
  QUIC backends. Padding is ordinary stream data, so every controller counts it
  and paces it. [Discover capacity](/quest/m2/quic-probe.md) keeps
  transport-level probing for a running encoder's increases, and may reuse this
  padding stream as its padding arm.
- **Only for a stopped rendition, in bounded bursts.** No padding while every
  rendition is running. A stopped rendition pads at its resume rate (the
  cutoff plus the 5% hysteresis), standing in for the traffic it would add on
  top of the media still sent, for about two seconds per burst. Bursts follow an exponential backoff, starting
  five seconds after the stop and doubling to a 60 s cap, and resuming resets
  it. A burst ends early when the estimate falls, since that means it found
  congestion. Padding runs at the lowest send priority, below all media.
- **The rate policy decides; the session pads.** `moq_mux::rate::Rung` and its
  JS mirror (`js/publish/src/rate.ts`) tell the publisher when to probe and up to
  what rate, under the same mocked time they already take. `moq-net` and
  `js/net` own the sender: the publisher asks the session to pad at a rate and
  gets back a handle, and dropping the handle stops the padding. No callbacks.
- **Wire: a moq-lite padding stream.** Add a unidirectional padding stream type
  to moq-lite that the receiver reads and discards, matching moq-transport's
  PADDING stream, which moq-net already discards
  (`rs/moq-net/src/ietf/session.rs`). A moq-lite peer that predates it resets
  the unknown stream without closing the session, as the spec requires, so the
  burst just ends and the rendition keeps today's behavior. Update
  `drafts/draft-lcurley-moq-lite.md` and the probe draft. On moq-transport
  sessions, send the existing PADDING type.

Open while implementing: the public shape of the session's padding handle
(name, and whether it takes a rate or a byte budget). Propose it with
alternatives in the PR.

Tests, with mocked time:

- `Rung`, Rust and JS: no probe while enabled. The first burst comes five
  seconds after the stop, then the backoff doubles to the cap and resets on
  resume. A falling estimate ends a burst. The burst rate is the resume rate.
- moq-net, under its simulated transport: the padding handle sends about
  rate × duration bytes on a padding stream at the lowest priority, then
  stops when dropped. Lite and IETF receivers discard it. A lite peer without
  the stream type resets it, and the session stays up.
- moq-quic end to end, in the seeded simulation (`rs/moq-quic/src/tests/`):
  extend `Bottleneck` to step its rate mid-run. A publisher sends 2.5 Mbit/s of
  media with a stopped 5 Mbit/s rendition while the path steps from 4 to
  10 Mbit/s. With padding, BBR3's estimate clears the resume rate within a
  bounded number of bursts. Without padding it never does: that is the
  regression this quest exists for. On a path held at 4 Mbit/s, the bursts
  never resume the rendition and media frame delay stays within budget.
- `js/net` and `@moq/publish`: a padding writer over a fake WebTransport under
  `@moq/clock`'s `Manual`, and an encoder test showing a stopped rendition
  probes and resumes once the fake estimate rises.
- One manual shaped-uplink run in Chrome, reported in the PR.

Wire these into CI, the moq-quic simulation at least nightly.

## Related

- [Simulcast rung disable](/quest/m2/simulcast-rung-disable.md) - the stopped renditions this lets resume
- [Discover capacity](/quest/m2/quic-probe.md) - transport-level headroom probing for a running encoder
- [Viewer up-switch](/quest/m2/viewer-upswitch.md) - the viewer-side counterpart, through PROBE
