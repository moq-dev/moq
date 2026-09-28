# m0: immediate priorities

## Goal

The work in flight now, in two independent tracks. Routing: a publisher stops
sending announce updates the wire cannot tell apart, a service claims the
prefix it could serve instead of enumerating broadcasts, and localhost workers
read only what their relay ingested. Audio playout: the target is a measured
estimate of arrival timing in both languages, a browser regression fails a
nightly run, and the audio playhead becomes the clock video follows.

## Plan

The release API gates (#3829..#3878) and the release that followed them are
done. moq.pro tracks this repository as a submodule rather than a release, so
no release quest gates this milestone. The Pronto GPU integration lives in
moq.pro.

Routing: announce-update dedupe is a wire-compatible fix on every version. The
wildcard line is prefix-only on the wire; its resolve and demand work is done
on the line branch and waits to land. Local origin serves the relay's
ingested-only view on the internal listener.

Audio playout: the jitter target replaces the round-trip guess. The harness's
browser lane grades it nightly and records the traces it replays; the native
lane is a standalone m1 quest, since nothing here waits on it. The A/V clock
builds on the jitter target's per-track spread.

Published API or wire breaks still land on dev; each quest's Plan says so.

## Required

- [Skip unchanged announce updates](/quest/m0/announce-update-dedupe.md) - a publisher sends an announce update only when the wire route changed
- [Wildcard](/quest/m0/wildcard/README.md) - a relay resolves subscriptions against advertised prefixes, a service claims the prefix it could serve and refuses the rest instead of enumerating broadcasts, and the browser player treats a covering claim as availability
- [Local origin](/quest/m0/local-origin.md) - localhost workers read only the broadcasts their relay ingested, from the internal listener
- [Audio quality harness](/quest/m0/audio-quality-harness/README.md) - a browser playout latency regression fails a nightly run instead of arriving as a bug report, and its recorder supplies the jitter target's replay traces
- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - the audio playout target is a measured estimate of arrival timing in both languages, not a round-trip guess
- [A/V clock](/quest/m0/plan-av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles

## Related

- [Pronto GPU integration](https://github.com/moq-dev/moq.pro/tree/main/quest/m0/pronto/gpu) - CARLA bridge, release adoption and desktop installation
