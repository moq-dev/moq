# [L] moq-cli: generic broadcast recording and paced replay (all tracks + catalog)

## Goal

Implement and verify the behavior tracked in [#2281](https://github.com/moq-dev/moq/issues/2281)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

"Where's my archive?" is the second question every broadcaster asks. There is no way to record a MoQ broadcast and no way to replay one.

Everything that looks like recording today is really *transcoding to a media container*, which is lossy in the ways that matter for an archive: it picks one rendition, drops the catalog, and can't round-trip.

#### What exists today

**Export is media-specific and stdout-only.** `moq <side> export <sink>` (`rs/moq-cli/src/args.rs`, `export` aliases `subscribe`). Sinks: `fmp4`, `mkv`, `ts`, `flv`, `h264`, `h265`, plus gateways `hls`/`rtmp`/`srt`/`rtc`. There is **no `--format` flag** (the format is the subcommand) and **no `--output <path>`**  -  everything writes `tokio::io::stdout()` (`rs/moq-cli/src/subscribe.rs`, `run_fmp4`/`run_mkv`/`run_ts`/...).

So `moq ... export fmp4 > out.mp4` works, but:

- it's a fragmented CMAF stream dumped to a file (no seekable `moov`), not a finalized recording;
- it's **one video + one audio rendition**, chosen via `--video-name`/`--audio-name`;
- the catalog drives the muxing and is then thrown away  -  it's never archived;
- nothing else on the broadcast survives (timeline, extra sections, non-media tracks, `mpegts` verbatim streams, future caption/cue tracks).

**No import from a file, either.** `ImportSource` container variants (`Avc3`, `Fmp4`, `Ts`, `Flv`) are **stdin only** (`rs/moq-cli/src/publish.rs`). You can `cat file.mp4 | moq ... import fmp4`, but with **no pacing**  -  it blasts the file at line rate instead of replaying at wall-clock speed. `ImportSource::Hls` (`rs/moq-cli/src/hls.rs`) accepts a local playlist path and does pace, which makes it the only file-ish input that behaves. No seek, no loop.

**Nothing persists groups to disk anywhere.** `cache::Pool` (`rs/moq-net/src/model/cache.rs`) is memory-only. Disk I/O in the tree is config, JWKS keys, and test fixtures.

#### What's missing

A **generic, media-agnostic broadcast archive**: all tracks, the catalog, group boundaries, and timestamps, such that replaying it reproduces the original broadcast. That's a different thing from an mp4, and it's the thing that matches MoQ's own design rule  -  a recorder shouldn't have to know what media is any more than a relay does.

The primitives are all sitting there unused:

- `rs/moq-json` has `stream` (lossless ordered append-log of self-contained records)  -  right shape for an index or a manifest.
- `rs/moq-mux/src/timeline.rs`'s doc comment **explicitly names "a recorder index" as a use case**: one `Record { group, pts }` per group per track. It's a group index with no payload, and nothing consumes it for this.
- `moq_mux::container::Producer::seek` / `pending_sequence` (`rs/moq-mux/src/container/producer.rs`) already exist, so replay can restore original group sequences rather than renumbering.

#### Proposed shape

An archive format that is essentially "the broadcast, on disk":

- catalog snapshots over time (it mutates  -  a rendition can appear or drop mid-broadcast);
- per track: group sequences, frame boundaries, timestamps, payload bytes, verbatim;
- a timeline/index for seeking without a full scan (the existing timeline record shape is the obvious candidate);
- no media parsing anywhere in the writer or reader.

Then:

- `moq ... export archive --output <path>` (and the missing `--output` for the existing sinks while we're here);
- `moq ... import archive <path>` that replays **paced to the media clock**, with `--loop` and `--start-at`. Paced replay is independently useful for testing, demos, and CI  -  the smoke tests currently have no way to replay a fixture at real speed.

Explicitly out of scope: turning an archive into an mp4. That's the existing exporters' job, and an archive should be losslessly convertible by piping it back through them.

#### Open questions

1. **Where does this live?** A `moq-archive` crate, or `moq-mux/src/container/archive`? It isn't really a media container  -  it's below that layer. Arguably it belongs closer to `moq-net`, since it only knows about tracks/groups/frames. But `moq-net` should probably not grow file I/O.
2. **Relationship to DVR.** A DVR needs a durable store behind `cache::Pool` that serves FETCH; an archive needs a file that replays. These are the same bytes and possibly the same format  -  worth designing together, or at least deciding they're deliberately separate. If the DVR store lands first, "record" might just be "point the store at a directory and never evict".
3. **Format**: a container-ish framing of our own, or reuse `moq-json` for the manifest plus opaque blobs for payloads? Prefer a maintained third-party container over hand-rolling if one fits, per the dependency rule  -  though "arbitrary tracks of timestamped opaque frames with group boundaries" may not map cleanly onto anything off the shelf. Matroska is the closest generic fit and would be worth a look before inventing one.
4. **Live-to-VOD**: does a completed archive get served back as a broadcast (via `OriginDynamic`/`request_broadcast`, which already serves broadcasts on demand that are never announced), or only replayed by the CLI? The former is much more interesting and is most of a VOD origin.
5. **Compaction**: an archive of a 24h broadcast is large. Does it drop non-selected renditions? That's a policy, not a format concern, but the format shouldn't preclude it.

#### Branch

`main`. New crate / new CLI subcommand / new sink are all additive. `--output` on the existing sinks is additive too.

#### Cross-package sync

`rs/moq-cli` → `doc/bin/cli.md`, and grep the repo for `moq ... export`/`import` examples per the CLI-docs rule. If it lands as a `moq-*` crate, `doc/lib/rs/`.

## Closes

- [#2281](https://github.com/moq-dev/moq/issues/2281) - close this issue when the quest finishes
