# [M] Fix gateway memory bounds, MIME parsing, disabled-track selection, and shutdown lifecycles

## Goal

WebRTC mux maps are bounded per session with O(1) teardown, WHIP/WHEP accept
parameterized SDP Content-Types, RTMP play bounds queued work and skips
disabled tracks, HLS clears waiting segments on clear/finish/cut, and
`Transcoder::run` exits when its source closes.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place high-severity and impactful correctness/leak
bugs in `m1` (rejected: `m0` with `release` backports; `m3`).

1. **`rs/moq-webrtc/src/server/mux.rs:96-110,146-160`**:
   - Bound `by_addr` / `claims` per session so a remote peer sending packets
     from spoofed/rotating addresses cannot grow the maps without bound, and
     clean up in O(1) on session drop.
2. **`rs/moq-webrtc/src/server/{whip.rs:229-235,whep.rs:209-215}`**:
   - Accept MIME parameters on `Content-Type` (e.g.
     `application/sdp; charset=utf-8`) instead of rejecting anything beyond an
     exact `"application/sdp"` string match.
3. **`rs/moq-rtmp/src/server.rs:765-861,970-1002,1425-1451`**:
   - Bound and drain `self.work` in `Play::accept` / `feed_input` so slow
     clients or bursts cannot accumulate unbounded pending work.
   - Filter the single-track branch of `play_selection` (`:970-1002`) with
     `.enabled()` so disabled tracks are never selected.
4. **`rs/moq-hls/src/export/renditions.rs:132,358-400,687-723`**:
   - Clear `state.waiting` in `clear()`, `finish()`, and `cut()` so stale
     waiting entries do not leak or stall segment publication across resets.
5. **`rs/moq-transcode/src/lib.rs:196-243`**:
   - Select on `self.source.closed()` in `Transcoder::run`'s main loop so a
     closed source terminates the transcoder promptly rather than hanging idle.

Public API: none. Wire: none. Add unit regression tests in `moq-webrtc`,
`moq-rtmp`, `moq-hls`, and `moq-transcode`.
