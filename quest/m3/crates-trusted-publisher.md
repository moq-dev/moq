# [XS] crates.io trusts the Rust release workflow

## Goal

Every crate that `release-rs.yml` publishes has a crates.io trusted publisher
for `moq-dev/moq`, workflow `release-rs.yml`, and environment `publish`, so
[Rust releases publish through OIDC](/quest/m3/release-rs-oidc.md) can start.
Delete this quest once every crate has one.

Corrected 2026-11-12 (/quest-audit): moq-bench leaves the list, since it
sets `publish = false`; seven crates remain. Corrected 2026-11-16
(/quest-plan of the pre-cut audit): moq-c leaves it too, since the
hand-written C API ships as `libmoq`, which has a trusted publisher, and
`moq-c` stays unpublished for the generated API; six remain. Corrected
2026-11-21 (/quest-plan of the pre-cut full-codebase audit): `moq-vaapi`
(vendored in [moq-dev/moq#8235](https://github.com/moq-dev/moq/pull/8235),
publishable and depended on by `moq-video`) joins the list; seven crates
remain. Corrected 2026-11-25 ([moq-dev/moq#9578](https://github.com/moq-dev/moq/pull/9578)):
`moq-opus`, the vendored Opus that `moq-audio` depends on, joins the list;
eight crates remain. Corrected 2026-11-25: `kio` and `kio-macros` moved to
[kixel-ai/kio](https://github.com/kixel-ai/kio), so their trusted publishers
belong to `kixel-ai/kio` (tracked in
[kixel-ai/kio `quest/m1/crates-io-publish.md`](https://github.com/kixel-ai/kio/blob/main/quest/m1/crates-io-publish.md))
and `moq-dev/moq`'s `release-rs.yml` no longer publishes them; seven crates
remain.

## Waiting

- Maintainer: after [the cut](/quest/m3/release/cut.md) publishes
  moq-congestion, moq-crypto, moq-opus, moq-quic, moq-rtsp, moq-vaapi, and moq-webrtc by
  token, add their trusted publishers the same way. Settings pages need a
  crates.io login, so check with a `release-rs.yml` run that publishes through
  OIDC.

## Plan

Decided 2026-11-05 (maintainer left the choice to the agent, /quest-unblock):
✅ this quest and [Rust releases publish through OIDC](/quest/m3/release-rs-oidc.md)
wait behind [the cut](/quest/m3/release/cut.md). The seven crates the cut
first-publishes by token cannot get a trusted publisher before it, and the cut
runs after m1, so neither quest could finish inside m1. Not chosen: keeping
both in m1 and treating the cut as part of finishing m1.


Moved from m2 to m1 on 2026-10-29 (maintainer): after
[moq-dev/moq#7166](https://github.com/moq-dev/moq/pull/7166) moved publish
secrets into the `publish` Environment, `CARGO_REGISTRY_TOKEN` is the
remaining long-lived registry token where npm and PyPI already use OIDC.

Decided 2026-10-22: the human step is split from the workflow change so it
stays visible as ready work. A crate that has never been published still needs
its first publish done by hand before it can have a trusted publisher; list any
such crate here.

Decided 2026-11-02 (maintainer, /quest-unblock): the maintainer is adding the publishers now,
each with repository `moq-dev/moq`, workflow `release-rs.yml`, and environment
`publish` (the job runs in it). Already on crates.io (30): hang, kio, libmoq,
moq-archive, moq-audio, moq-auth, moq-boy, moq-cli, moq-ffi, moq-gst, moq-hls,
moq-json, moq-loc, moq-msf, moq-mux, moq-native, moq-net, moq-nvenc,
moq-pattern, moq-relay, moq-rtmp, moq-sock, moq-srt, moq-stats,
moq-tokio, moq-transcode, moq-uring, moq-v4l, moq-video, qmux. Not yet
published (7), so their first publish needs the token and they get a trusted
publisher after it: kio-macros (moved to `kixel-ai/kio` on 2026-11-25, along
with `kio`), moq-c, moq-congestion, moq-crypto,
moq-quic, moq-rtsp, moq-webrtc. moq-crypto became publishable in
[moq-dev/moq#7733](https://github.com/moq-dev/moq/pull/7733). `CARGO_REGISTRY_TOKEN` is revoked only after all of them have one.
Settings pages need a crates.io login, so the check is a `release-rs.yml` run
publishing through OIDC.

Done 2026-11-04 (maintainer, /quest-unblock): all 31 published crates have
the trusted publisher. Only the unpublished crates remain: as of 2026-11-12
crates.io has none of the seven in Waiting.

Decided 2026-11-05 (maintainer, /quest-audit): moq-e2ee, published but
retired in favor of moq-crypto, gets no trusted publisher;
a later change took it out of the
workspace instead. Reversed 2026-11-16 (maintainer, /quest-plan of the
pre-cut audit): moq-e2ee is a crate again, unpublished at the cut
([the crate move](https://github.com/moq-dev/moq/pull/8947)), and gets its trusted publisher
when [the finished format](/quest/m3/e2ee-format.md) publishes it, which
that quest's `Waiting` item tracks.

## Required

- [Cut the release](/quest/m3/release/cut.md) - the cut first-publishes these crates by token
