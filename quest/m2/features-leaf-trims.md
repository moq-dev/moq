# [S] Drop the compile-trim features from the media and gateway crates

## Goal

These features, each gating pure-Rust code or logic only, go away and
their code always compiles:

- moq-audio: `aac` and `bus` (pure Rust, on by default, and every consumer
  that builds moq-audio enables `aac`), and `aec`, which adds no dependency and
  gates only the echo-canceller wiring between `capture` and `playback`;
  compile that on `all(feature = "capture", feature = "playback")`.
- moq-ffi: `audio` and `video`, which are defaults no build turns off (the C++
  wrapper refuses a prebuilt moq-ffi without them).
- moq-rtmp: `tls`, whose rustls and tokio-rustls every consumer already has
  through moq-tokio.
- moq-webrtc: `p2p`, whose if-addrs, mdns-sd, and qmux are already in moq-cli's
  tree through LAN discovery and the qmux transports.
- moq-quic: `mux`, which gates the sans-IO QMux connection and no dependency.
- moq-video: `dmabuf`, a private helper module the `vaapi`, `render`, and
  `pipewire` features turn on; compile it on Linux.

And moq-video's `v4l2`, which costs the build nothing (moq-v4l checks its
bindings in), joins the defaults beside `vaapi` and `nvidia` in moq-video,
moq-transcode, and moq-cli.

## Plan

Found by the [Cargo feature review](/quest/m3/release/feature-flags.md) (first
pass, 2026-11-14), against the `# Features` rules in `rs/AGENTS.md`: none of
these is a vendor integration or a system dependency, so each only trims
compile time or gates logic out of the default test run.

moq-video's `v4l2` stays a feature rather than always compiling, like `vaapi`
and `nvidia`: each is one vendor's hardware path, and a consumer such as
demo/carla's bridge builds one vendor alone. As a default, the moq-cli
release workflow's `linux-features: v4l2` becomes redundant;
[Ship capture and playback](/quest/m3/cli-packaging.md) replaces that line
with `full` anyway, so leave it to that quest.

Breaking for anyone who names a removed feature: the PR's Impact section says
to delete it. Update `doc/lib/rs` pages and crate READMEs that mention these
features, and `rs/justfile`'s `--features aec,pulseaudio` test line.

## Related

- [Always compile the network stack](/quest/m2/features-network.md) - the same
  rule applied to the transport crates
