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
BBR3 only raises `max_bw` from an app-limited sample above its current value,
and only ages its window on a cycle that isn't app-limited (`update_max_bw`
and `adapt_long_term_model` in `rs/moq-quic/src/congestion/bbr3/mod.rs`).
Chromium's QUICHE gates its BBR2 filter the same way, and its Cubic estimate
stops growing while the sender isn't cwnd-limited. So after a rendition
stops, the estimate freezes at the value that stopped it, and today nothing
makes it rise: moq-net advertises PROBE `Report` only.

Decided with the maintainer on 2026-10-17 (quest-plan on
[#5863](https://github.com/moq-dev/moq/pull/5863)), replacing that PR's
publisher-local padding handle:

- **One session target.** The session probes to the larger of its local
  target and the peer's PROBE target. PROBE `Increase` is the wire signal;
  retransmission or padding is the mechanism. The
  [target](/quest/m2/probe/target.md) quest owns it.
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
- **Native first, padding as the fallback.** Where moq-quic sends, it
  retransmits in-flight data during BBR3's ProbeBW_UP phase
  ([native](/quest/m2/probe/native.md)). Where the stack can't (browsers over
  WebTransport, other QUIC backends), a lowest-priority padding stream on an
  ALR probe schedule does ([padding](/quest/m2/probe/padding.md)).

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

- [Session probe target](/quest/m2/probe/target.md) - a session probes up to its reservations or the peer's PROBE target, sends its viewer handles as its own PROBE target, and advertises `Increase` where a sender exists
- [Native probing](/quest/m2/probe/native.md) - moq-quic retransmits in-flight data during BBR3's ProbeBW_UP so an app-limited sender still validates headroom up to the target
- [Padding probes](/quest/m2/probe/padding.md) - a lowest-priority padding stream probes on an ALR schedule where the stack can't retransmit, in browsers and on other QUIC backends
- [Players set viewer targets](/quest/m2/probe/players.md) - players hold a viewer handle per watched broadcast at its highest enabled rendition's bitrate
- [Simulcast rung disable](/quest/m2/probe/simulcast-rung-disable.md) - JS, Rust/FFI, and OBS publishers stop encoding the top renditions their grant cannot fund, advertise `enabled: false`, and re-enable with hysteresis
- [Viewer up-switch](/quest/m2/probe/viewer-upswitch.md) - a viewer capped by its small rendition finds headroom through PROBE and moves up

## Related

- [A finished PROBE stops reporting](/quest/m2/probe-end-stale.md) - the receive estimate this epic raises reads unknown once its stream ends
- [BBR media study](/quest/m2/quic-bbr-natural-drain.md) - preserving an estimate and discovering spare capacity are separate problems
- [Realtime uplink congestion control](/quest/m2/quic-realtime/README.md) - a delay-based alternative to BBR3 on native uplinks, with no ProbeBW_UP phase to ride
