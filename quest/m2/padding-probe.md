# [L] Publishers can pad to find room above their media

## Goal

A publisher can ask its session for short, low-priority padding bursts at a
chosen rate, on a bounded backoff schedule, so the congestion controller
measures room above the media the publisher is sending. When the uplink has
room, the bandwidth estimate rises past media plus the burst rate. When it
doesn't, the padding stays within its budget and never delays media. This
holds for native publishers (moq-net over any QUIC backend) and browser
publishers (`js/net` over WebTransport). The first user is a simulcast
rendition stopped for lack of bandwidth, which
[rung disable](/quest/m2/simulcast-rung-disable.md) resumes with it.

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
- **This quest builds the mechanism; rung disable wires it in.** `moq-net` and
  `js/net` own the sender: a publisher asks the session to pad at a rate and gets
  back a handle, and dropping the handle stops the padding. No callbacks. The
  burst schedule (backoff, burst length, early stop on a falling estimate) is a
  policy-agnostic type in `moq_mux::rate`, mirrored in `js/publish/src/rate.ts`,
  taking mocked time like `Control`. It knows nothing about renditions.
  [Simulcast rung disable](/quest/m2/simulcast-rung-disable.md) (#5850) then
  drives it from `Rung` and the publishers once this lands, so neither quest
  waits on the other's code.
- **Wire: a moq-lite padding stream.** Add a unidirectional padding stream type
  to moq-lite that the receiver reads and discards, matching moq-transport's
  PADDING stream, which moq-net already discards
  (`rs/moq-net/src/ietf/session.rs`). A moq-lite peer that predates it resets
  the unknown stream without closing the session, as the spec requires, so the
  burst just ends and the rendition keeps today's behavior. Update
  `drafts/draft-lcurley-moq-lite.md` and the probe draft. On moq-transport
  sessions, send the existing PADDING type only from draft-18 on. Drafts 14
  to 17 treat it as an unknown stream type and close the session
  (`UniType::classify`), so the handle reports padding as unsupported there
  and the rendition keeps today's behavior.

Open while implementing: the public shape of the session's padding handle
(name, and whether it takes a rate or a byte budget). Propose it with
alternatives in the PR.

Tests, with mocked time:

- The burst schedule, Rust and JS: the first burst comes five seconds after
  it starts, then the backoff doubles to the cap and resets when stopped. A
  falling estimate ends a burst.
- moq-net, under its simulated transport: the padding handle sends about
  rate × duration bytes on a padding stream at the lowest priority, then
  stops when dropped. Lite and IETF draft-18 receivers discard it. A lite peer
  without the stream type resets it, and the session stays up. On drafts 14 to
  17 the handle refuses to pad, and nothing is sent.
- moq-quic end to end, in the seeded simulation (`rs/moq-quic/src/tests/`):
  extend `Bottleneck` to step its rate mid-run. A sender sends 2.5 Mbit/s of
  media while the path steps from 4 to 10 Mbit/s, and pads 3.5 Mbit/s bursts
  on the schedule. With padding, BBR3's estimate rises past media plus the
  burst rate within a bounded number of bursts. Without padding it never does:
  that is the regression this quest exists for. On a path held at 4 Mbit/s,
  the estimate stays put and media frame delay stays within budget.
- `js/net`: a padding writer over a fake WebTransport under `@moq/clock`'s
  `Manual`.
- One manual shaped-uplink run in Chrome, padding over a constrained uplink,
  reported in the PR. The stopped-rendition run belongs to rung disable.

Wire these into CI, the moq-quic simulation at least nightly.

## Related

- [Simulcast rung disable](/quest/m2/simulcast-rung-disable.md) - the stopped renditions this lets resume
- [Discover capacity](/quest/m2/quic-probe.md) - transport-level headroom probing for a running encoder
- [Viewer up-switch](/quest/m2/viewer-upswitch.md) - the viewer-side counterpart, through PROBE
