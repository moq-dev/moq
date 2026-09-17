# [L] moq-stats takes an extension and hang defines the media stats

## Goal

`moq_stats::Producer<E>` publishes a stats broadcast whose per-broadcast
entries are `Traffic` plus an extension `E`, `Consumer<E>` and the aggregate
read it back, any consumer can request one broadcast's entry as its own
track, and `hang::Stats` is the media extension: what a publisher sent and
what a subscriber received and played, per audio and video. The relay is
`Producer<()>` and its wire output does not change.

## Plan

- `moq_stats::Ext`: `Serialize + DeserializeOwned + Default + Clone + Merge`,
  implemented for `()`. `Merge` is the aggregate's private `Mergeable` made
  public with the `Copy` bound dropped; `Traffic` and `Presence` implement it
  as they do today. A frame is `BTreeMap<String, Stats<E>>`, where `Stats<E>`
  is `Traffic` with `E` flattened beside it; `TrafficFrame` becomes that
  alias for `()`. `sessions.json` stays a core track outside the extension:
  `[<tier>/]sessions.json[.z]`, a `BTreeMap<String, Presence>` keyed by auth
  root, read through `Consumer::sessions` whatever `E` is; a client publishes
  it only when it holds sessions worth counting.
- An exact-path mode. `ProducerConfig` treats its path as a prefix and
  advertises `<prefix>/node[/<node>]`, so a client asking for
  `room/alice.stats` would publish `room/alice.stats/node`, which no longer
  ends in `.stats`. `ProducerConfig::at(path)` publishes the broadcast at
  exactly that path with no category segment, refusing a path that does not
  end in `.stats`; the relay keeps the prefix layout. Test the advertised
  path for both modes.
- Per-broadcast tracks: `requested_track_shape` in
  `rs/moq-stats/src/produce.rs` accepts
  `[<tier>/]<path>/{publisher,subscriber}.json[.z]` by matching the
  producer's tier labels longest first, and refuses a default-tier path that
  starts with another tier's label. The track is a snapshot of that one
  entry, emitted on the same interval, and lives while requested under the
  existing quotas. `Consumer<E>` gains `traffic_for(tier, role, path)`.
- `hang::Stats { audio, video, transport }` in `rs/hang/src/stats.rs`, one
  struct per role folded into sections, every field defaulted and unknown
  fields ignored:
  - subscriber, per audio and video: bytes and frames received, frames
    decoded, frames dropped as late, stalls and stalled duration, audio
    underruns, decode errors, the newest media timestamp received and the
    wall time it arrived (liveness: exact in media time, as good as the
    clock in wall time), and the current playout latency as a gauge;
  - publisher, per audio and video: frames, bytes, keyframes, frames dropped
    before encode, and the target bitrate as a gauge;
  - publisher `transport`: rtt, estimated send rate, bytes and packets lost,
    and sample age from `ConnectionStats`, repeated across the broadcasts one
    connection carries.
  `Merge` sums counters, leaves gauges out of the sum, and keeps the newest
  liveness pair.
- Naming: `.stats` as a broadcast suffix, documented in the `moq-stats`
  crate docs and `doc/lib/rs/index.md` beside the relay's prefix convention,
  with `moq_stats::is_stats(path)` so a dashboard filters telemetry from
  content.
- The relay keeps `Producer<()>`; the `[stats]` section of
  `doc/bin/relay/config.md` gains the per-broadcast track and its refusal
  rule. Fixtures: a relay frame parses as `Stats<()>` and as
  `Stats<hang::Stats>` with the extension defaulted; a media frame parses
  under the old `Traffic` with the extension ignored; the aggregate sums two
  clients' counters and leaves gauges alone; the per-broadcast track for a
  tiered name and a refused ambiguous one.

## Related

- [Starvation](/quest/m2/qos/starvation.md) - the relay's delivery counters,
  the other half of a health verdict
