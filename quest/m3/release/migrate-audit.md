# [M] Migration audit of the breaking cut

## Goal

Before the breaking cut merges, every break it ships, including those in
surfaces `just api-diff` cannot see, has its
`MIGRATE.md` entry, and each entry states the net change from the last
published version.

## Plan

Recurs once per API or wire cut (decided 2026-11-09). The [release
cut](/quest/m3/release/cut.md) re-creates this quest when the maintainer
names the next API or wire cut, so the PR finishing this run adds no
successor. Only `/quest-delete` ends the recurrence.

Decided 2026-10-16 (maintainer interview); the diff tooling moved to its own
m1 quest on 2026-10-22.

- PRs into `main` no longer write `MIGRATE.md` entries (decided
  2026-10-23). Gather the breaks from `!` PRs: every `!` subject in
  `git log` since the `release` merge base, then each PR's Impact section
  for its migration note, since squash commits drop the PR body. Write each
  note into its package's guide. The API-diff report below catches breaks a
  PR left unmarked; a behavior break with no `!` stays with PR review.
- Correct Unreleased entries that a later PR overturned. Known, from
  [moq-dev/moq#7036](https://github.com/moq-dev/moq/pull/7036)'s rename of
  `used` to `wanted` (Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups)): `rs/moq-json/MIGRATE.md`
  should say `producer.demand().wanted().is_some()`, not
  `producer.demand().is_used()`. In `js/net/MIGRATE.md`, the line saying
  broadcast and group demand keep `used` and `unused()` is false, the line on
  `unused()` rejecting on close is moot, and
  "`producer.unused()` is `producer.demand().unused()`" should point at
  `demand().wanted`.
  And `rs/moq-auth/MIGRATE.md`'s "Mesh endpoints use the signing public
  key" names `moq_crypto::sign`, which
  [moq-dev/moq#8855](https://github.com/moq-dev/moq/pull/8855) deleted, for
  `moq_auth::mesh`, which
  [moq-dev/moq#8756](https://github.com/moq-dev/moq/pull/8756) removed:
  drop it, or correct it against the published moq-auth. `sh/migrate.sh`
  refuses that edit outside the cut, so
  [moq-dev/moq#8947](https://github.com/moq-dev/moq/pull/8947) left it here.
  Drop `rs/moq-net/MIGRATE.md`'s "Origins prove a route key" too: no
  release had `origin::Config::route_key` or
  `poll::Session::export_keying_material`, and
  [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) and
  [moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053) removed
  them.
  And both `js/net/MIGRATE.md` and `rs/moq-net/MIGRATE.md` say, under "A
  timed track refuses a group that rewinds (#5803)", that a track received
  over moq-transport stays unchecked, which
  [moq-dev/moq#9028](https://github.com/moq-dev/moq/pull/9028) overturned:
  a group received over moq-transport is checked too, since its time is its
  Group ID, and only a moq-lite-05/06 copy keeps its timestamps unchecked,
  so "serves both on" becomes "serves it on".
- Run the API-diff report on the cut PR and read the diff of the
  hand-written binding wrappers (`py/moq-rs`, `kt/moq`, `dart/moq`,
  `go/wrapper`, `swift`, `cpp/moq`, `cpp/obs`) and of each binary's flags,
  env vars, and config keys by hand. Add this manual pass to
  RELEASE.md's Releases section, which already names the other sources.
- Fill every gap the report and the manual pass show in the matching
  `MIGRATE.md`. Known gaps: breaking releases cut after the
  2026-09-23 train have no transition section yet (moq-json 0.4 -> 0.5
  and possibly moq-audio and moq-video), because their breaks predate the
  backfill's merge base. The [compression migration](https://github.com/moq-dev/moq/pull/7046)
  deleted moq-flate before this cut: audit the migration from its published
  API to ordinary tracks with `compression`, rather than promising a new
  moq-flate transition release.
- Each version-transition section, starting from the `## Unreleased`
  entries already on `main`, says how to migrate from the published
  version to the candidate (decided 2026-10-22, maintainer, from an external
  API audit). Collapse entries a later change reversed or reshaped (an API
  removed then reintroduced, a default changed twice) into the net change,
  and drop the intermediate history.
- Read the API-diff report's auto-trait changes too, not only renamed or
  removed items (added 2026-10-27, from an external release-to-main audit
  that found lost `Sync` on JSON consumers, fMP4/HLS importers, video decode
  consumers, and relay state). Restore an accidental loss at its source;
  otherwise the migration note says how to share the type instead.
- Compile-check the code in every `MIGRATE.md` section the cut ships against
  the candidate. Nothing else builds it.
- Required by the cut, after m2's breaking work has landed.
- Start from the candidates [moq-dev/moq#6477](https://github.com/moq-dev/moq/pull/6477)
  listed as breaks matching no name in `MIGRATE.md` (about 15 Rust crates,
  `@moq/hang`, `@moq/net`, `@moq/watch`, and moq-ffi): confirm each, and add
  its migration note or record why none is needed.
- Known break, a Nix option rather than a Rust API (added 2026-10-31, from
  [moq-dev/moq#7343](https://github.com/moq-dev/moq/pull/7343)): the NixOS
  module option `services.moq-relay.cluster.rootUrl` takes a non-empty
  list instead of a string, and is renamed `cluster.connect`
  ([moq-dev/moq#8924](https://github.com/moq-dev/moq/pull/8924)). Migrate
  `rootUrl = "https://root:4443";` to `connect = [ "https://root:4443" ];`;
  an empty list fails evaluation, and `rootUrl` fails it naming `connect`.
  Write it into `rs/moq-relay/MIGRATE.md`'s transition section (its
  `## Unreleased` section until the cut names the version).

- Known drift (added 2026-11-07, from the API shape audit), to fix in the
  guide each names:
  - moq-webrtc: `rs/moq-webrtc/MIGRATE.md` is still titled "Migrating
    moq-rtc" and documents `wire::Source`, which
    [moq-dev/moq#6454](https://github.com/moq-dev/moq/pull/6454) removed.
  - moq-cli: `rtmp` is listed under `--max-delay` but its flag is `--delay`;
    the `--connect-once` removal and the `MOQ_LISTEN_TLS_*` to
    `MOQ_HLS_TLS_*` rename are missing; the `export archive` entry reads as a
    break of something release never had.
  - moq-relay: the line saying no transport enforces bitrate caps is stale
    (QUIC enforces them through the pacer); the two `receive_window` floor
    entries (64 MiB, then 1 MiB) collapse into the net change; and the
    removed `moq_relay_stale_datagrams_total` metric has no note.
  - moq-gst: the request pad's `encoder` property, shipped in
    `moq-gst-v0.4.24`, was removed with no entry, so a `gst-launch` line
    setting it now fails at runtime.
  - moq-archive (added 2026-11-20, from the main → release audit):
    `rs/moq-archive/MIGRATE.md`'s "Recording format version 4 drops the
    datagram form" and "Recording format version 3 replaces version 1" are
    wrong since [moq-dev/moq#8932](https://github.com/moq-dev/moq/pull/8932):
    `VERSION` is 2, the only version written or read, and `Error::Form` is
    gone. Replace both with #8932's Impact text: format version 2 replaces
    version 1; start a new recording prefix; main builds' versions 2 to 4 are
    unsupported; `Info::version` and `Error::Form` are removed and `Info` no
    longer implements `Serialize`/`Deserialize` (use `Info::encode` and
    `Info::decode`); keep a release build's reader for retained version 1
    archives. Keep the `i8` priority and optional timescale notes, and check
    the "Objects are keyed by timeline segment" entry's `Key::Segments`,
    `Store::get_segments`, and `Error::Span`, none of which exist on `main`.

## Required

- [Breaking release readiness](/quest/m2/README.md) - all breaking work and the MIGRATE.md convention have landed
