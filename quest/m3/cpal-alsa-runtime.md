# [S] A cpal release loads libasound at runtime

## Goal

A cpal release on crates.io whose Linux build no longer makes libasound a
load-time requirement, either by loading it at runtime (dlopen) or by putting
the ALSA host behind a feature that the PulseAudio host can replace. Once the
release is out, delete this quest.

## Waiting

- RustAudio/cpal: ships that release, as asked in
  [RustAudio/cpal#1421](https://github.com/RustAudio/cpal/issues/1421) (or
  diwic/alsa-sys, if the loading lives there). Check: build a capture and
  playback `moq-cli` against that release (with runtime loading,
  `--features capture`; with an optional host, cpal's ALSA host off and
  `pulseaudio` on) and confirm `readelf -d` lists no `libasound.so.2` under
  `NEEDED`. A clean `readelf` is not enough on its own: the binary must also
  start, list devices, and play on a host without libasound, as
  [capture-alsa-link](/quest/m3/capture-alsa-link.md) verifies.

## Plan

Moved from m5 to m3 2026-11-13 (maintainer, /quest-plan):
[cli-packaging](/quest/m3/cli-packaging.md) in m3 requires it, and no quest
requires one in a later milestone. ✅ m3 (rejected: moving cli-packaging and
nixpkgs-cli to m5, which waits for the cut).

Decided 2026-10-08: the capture chain (this quest,
[capture-alsa-link](/quest/m3/capture-alsa-link.md), and
[Ship capture and playback](/quest/m3/cli-packaging.md)) parks until cpal
ships runtime ALSA loading, with no fork taken meanwhile.

Moved to m5 2026-11-12 (maintainer, /quest-audit): it waits only on an upstream cpal release. Superseded 2026-11-13: it is back in m3, per the decision above.

Filed 2026-11-02 (maintainer-approved, /quest-unblock): [RustAudio/cpal#1421](https://github.com/RustAudio/cpal/issues/1421), recommending runtime loading first (most naturally in alsa-sys) and an optional ALSA host second. Related, not duplicates: RustAudio/cpal#1404 (FreeBSD-only `alsa` feature), cpal#929, and diwic/alsa-sys#10 (static linking).

State as of 2026-11-12: #1421 is open. A cpal maintainer replied on
2026-11-04 that an optional ALSA host would break existing builds and that
`default_host()` would then have no host to return on a headless Linux
build, and asked whether static linking would do; nothing is decided. cpal's
latest release is still 0.18.2 (2026-08-16), the version pinned here.

State as of 2026-10-05: cpal 0.18.2 and cpal master (0.19.0)
both depend on `alsa` unconditionally on Linux and the BSDs. `alsa` 0.12.1
depends on `alsa-sys` 0.6.1, whose build script links libasound through
pkg-config and offers no runtime-loading mode.

Two upstream shapes, either of which unblocks
[Audio capture without runtime system libraries](/quest/m3/capture-alsa-link.md):

- **Runtime loading.** An `alsa-sys` feature that resolves the `snd_*`
  symbols through dlopen (bindgen can emit such a wrapper), forwarded by
  cpal, with the ALSA host reporting itself unavailable when libasound is
  missing so host selection falls through. Keeps raw ALSA on hosts without a
  sound server.
- **Optional ALSA host.** A default-on cpal feature for the ALSA host. cpal's
  `pulseaudio` host is a pure-Rust protocol client, so a build with ALSA off
  and `pulseaudio` on needs no system audio library, but loses devices on
  hosts that run no sound server.

Runtime loading matches the vaapi and nvidia pattern and the quest's
fall-through goal, so prefer it.

Fallback if upstream declines: fork `cpal`, `alsa`, and `alsa-sys` as
published crates, as moq-v4l was. It is the only in-tree route that reaches
`cargo install moq-cli`, at the cost of carrying three forks for one library;
the maintainer decides if it comes to that.

Rejected in-tree routes, for whoever retries this: a `[patch.crates-io]` fork
of `alsa-sys` does not reach `cargo install moq-cli` or crates.io consumers
of moq-audio, and defining the `snd_*` symbols in moq-audio so `--as-needed`
drops libasound interposes them process-wide and breaks silently when cpal
calls a symbol the shim misses.

## Related

- [Ship capture and playback](/quest/m3/cli-packaging.md) - needs the microphone path to start without libasound
