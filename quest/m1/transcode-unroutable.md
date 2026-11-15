# [S] A transcode derivative is sometimes unroutable

## Goal

A viewer subscribing to a running `moq transcode` derivative reaches it every time, not only on some runs.

## Plan

Seen 2026-11-14 on a loopback relay (the `test/portrait` relay config, moq-lite-07-wip): `moq ... --broadcast src.hang import avc3` from ffmpeg, `moq ... --broadcast src.hang transcode --encoder software --decoder software`, then, 8 s after the transcoder logged `transcoding`, two `moq ... --broadcast src.hang/transcode.hang export --video-name video/240p h264` viewers.
In about one run of three, both viewers failed with `unroutable`, and the relay logged `subscribed error broadcast=src.hang/transcode.hang track=catalog.json err=unroutable`; the other runs played, though some logged one `unroutable` first.
It happened with no `--encoder-sessions` budget, on a branch based on `7694ffaed`.

The CLI routes the derivative with `RouteOptions::hidden()` and announces it once its transcoder exists (`rs/moq-cli/src/transcode.rs`), and the CLI logged no new source instance while the viewers failed.
Reproduce it in a `moq-net` or relay test before fixing, and fix the root cause rather than retrying the subscribe.
