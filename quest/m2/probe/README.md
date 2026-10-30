# Probe up to a target bitrate

## Goal

A session occasionally probes its connection up to a target bitrate, so the
congestion estimate can rise to fund what the session wants to send. A
simulcast rendition stopped for lack of bandwidth can resume, and a viewer
capped by a small rendition can up-switch. It works with several broadcasts
on one connection, their targets summed transparently, and through origin
fan-out, each hop probing for itself. Our publishers and players set targets
by default; an application can override or disable them.

## Plan

The estimators stop learning once a connection is app-limited. moq-quic's
BBR3 only raises `max_bw` from an app-limited sample above its current value;
while app-limited it ages its window every 3 s, so the estimate falls to what
the flow still delivers but never rises past it (`update_max_bw` in
`rs/moq-congestion/src/bbr3/mod.rs`). Chromium's QUICHE gates its BBR2 filter
the same way without the aging, and its Cubic estimate stops growing while the
sender isn't cwnd-limited. So after a rendition stops, the estimate settles at
or below the value that stopped it, and today nothing makes it rise: moq-net
advertises PROBE `Report` only.

Decided with the maintainer on 2026-10-17 (quest-plan on
[#5863](https://github.com/moq-dev/moq/pull/5863)), replacing that PR's
publisher-local padding handle:

- **One session target.** The session probes to the larger of its local
  target and the peer's PROBE target. PROBE `Increase` is the wire signal;
  retransmission or padding is the mechanism. The
  send target (`bandwidth::Allocator`'s reservations) owns the local half and
  [viewer targets](/quest/m2/probe/viewer-target.md) the peer's.
- **Viewers are in scope.** The viewer up-switch joined the epic, since a
  viewer's target is the same session target sent the other way.
- **moq-net stays catalog-agnostic, and each hop is independent.** A relay
  never forwards a target upstream, and there is no per-track wire field.
  Every hop already runs its own congestion controller, so aggregating
  targets upstream would couple hops for no gain; a publisher learns what to
  fund from its own reservations, a viewer from its player.
- **Automatic, opt-out.** Our publishers (through their reservations) and
  players set targets by default, so probing works without an application
  knowing about it.
- **Native retransmits, the browser pads.** Where moq-quic sends, it
  resends in-flight data during BBR3's Startup and one ProbeBW_UP per probe
  wait ([moq-dev/moq#6635](https://github.com/moq-dev/moq/pull/6635)). A browser over WebTransport can't
  reach its controller, so a lowest-priority padding stream on an ALR probe
  schedule does ([padding](/quest/m2/probe/padding.md)).

Decided with the maintainer on 2026-10-20:

- The send target, native probing,
  and the native publishers' [rung disable](/quest/m1/simulcast-rung-disable.md)
  moved to m1, so Rust, FFI, and OBS publishers recover a stopped rendition in
  the next breaking release. Their rung disable needs the target and native
  probing, not padding.
- Padding is browser-only (WebTransport in `js/net`). moq-quic with BBR3 is
  the only native backend with a probing hook; other native backends (iroh
  on noq, moq-quic under CUBIC) have no sender, keep `Report`, and never
  disable a rung. lite-07 reserves its stream
  type before the cut (Padding Stream 0x2), and
  the sender stays here.
- The JS publisher's rung disable split into its own quest here, since it
  probes with padding.
- The target then split: m1 keeps the send target
  native rung recovery needs, and viewer handles, viewer grants, the peer's
  PROBE target, and `Increase` came back here as
  [viewer targets](/quest/m2/probe/viewer-target.md), beside their consumers.
  The `js/net` send target moved to [padding](/quest/m2/probe/padding.md), its
  only sender.

This README's own work, once the children land:

- An end-to-end shaped-uplink test, at least nightly: a Chrome publisher
  whose top rendition stopped under a constrained uplink resumes when the
  uplink opens, and a `@moq/watch` viewer on a small rendition up-switches
  when its downlink opens. Both through a relay, so fan-out is covered.
- Docs: probing is automatic and changes what a user would do (opt out,
  override a target), so describe it where bandwidth adaptation is already
  covered, editing the PROBE paragraph in `doc/concept/standard.md` and the
  `doc/lib` pages rather than adding a page, unless the material outgrows
  them. Children keep their own API docs and drafts current inline.

## Required

- [Native rung disable](/quest/m1/simulcast-rung-disable.md) - OBS stops sending the top renditions its grant cannot fund, as the Rust/FFI publishers do, and a shaped uplink through moq-ffi proves the resume
- [Padding probes](/quest/m2/probe/padding.md) - a browser probes with a lowest-priority padding stream on an ALR schedule
- [JS rung disable](/quest/m2/probe/rung-disable-js.md) - `@moq/publish` applies the native rung rule, recovering through padding
- [Viewer targets](/quest/m2/probe/viewer-target.md) - a session sends its viewer handles as its PROBE target, probes to the peer's, splits the receive estimate into viewer grants, and advertises `Increase` where a sender exists
- [Players set viewer targets](/quest/m2/probe/players.md) - players hold a viewer handle per watched broadcast at its highest enabled rendition's bitrate
- [Viewer up-switch](/quest/m2/probe/viewer-upswitch.md) - a viewer capped by its small rendition finds headroom through PROBE and moves up

## Related

- [BBR media study](/quest/m2/quic-bbr-study.md) - preserving an estimate and discovering spare capacity are separate problems
- [Realtime uplink congestion control](/quest/m2/quic-realtime/README.md) - a delay-based alternative to BBR3 on native uplinks; it must implement native probing to ship (decided 2026-10-25)
