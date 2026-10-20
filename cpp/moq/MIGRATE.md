# Migrating the C++ moq library

How to update your code for each breaking change, newest first.

## Unreleased

### Ranges filter reads, and anchors can name a time (#5929)

A subscription now reads only the groups its own `ranges` cover.
`Anchor` gains a `Time` case holding `timestamp_us`, the group containing that presentation time, so exhaustive visits need a case for it.
`TrackConsumer::update` now returns a result, failing with `TimestampMismatch` on a time anchor an untimed track can't serve.

### Track demand reports the wanted group (#5930)

`TrackDemand::is_used`, `used`, and `unused` are gone.
`wanted()` returns `std::nullopt` while nobody subscribes, otherwise the group subscribers want next, and `wanted_changed()` waits for it to change (the first call returns at once).
Loop on `wanted_changed()` until it returns a value where you awaited `used()`, and `std::nullopt` where you awaited `unused()`.
`TrackInfo` gains `refresh`, the ratio past which a joining subscriber asks for a new group.

### `CongestionControl` gained `kNone` (#5926)

`moq::CongestionControl::kNone` runs no congestion control and no pacing, for tests and controlled links.
A `switch` without a `default` needs one more case.

### Container byte streams use `MediaContainerProducer` (#5919)

`moq::MediaContainerStreamProducer` and `moq::ContainerStreamInit` are gone: a container recovers its own framing, so `moq::MediaContainerProducer` takes chunks split anywhere.
Pass empty `data` and write the stream through it.

```cpp
auto stream = ok(moq::MediaContainerProducer::init(broadcast, {moq::ContainerFormat::kFmp4, {}}));
```

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Set `QuicConfig::keylog` to `true` as well to capture them; without `SSLKEYLOGFILE` the client or server refuses to build.

### Subscriptions carry group ranges (#5732)

`Subscription::group_start` and `group_end` are replaced by `ranges`, a list of `Range` whose ends are an `Anchor` (an absolute group and frame, or a count of groups before the latest), and an optional `order`.
To start at group `n`, pass one range starting at the absolute anchor for group `n`, frame 0.

### Receive estimates come from `Session::recv_bandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `Session::recv_bandwidth` returns to request PROBE, read it with `peek()` or wait with `changed()`, and `cancel()` it to stop.

```cpp
auto bandwidth = session->recv_bandwidth();
auto rate = bandwidth ? bandwidth->peek() : std::nullopt;
```

### `AnnouncedBroadcast::available()` waits for the next publisher instance (#5700)

The first call returns the broadcast serving the path, as before.
Each later call waits for the next instance to take over the path, instead of returning whatever serves it now.

### A timed track refuses a group that rewinds (#5803)

Writing a frame fails with `TimestampMismatch` when a group's first frame starts before a lower-sequence group's, or after a higher one's.
A publisher whose timeline restarts publishes a new broadcast instead.

### Bandwidth reserves a track's demand (#5845)

`Bandwidth::reserve` takes the track's `demand()` instead of the producer, and no longer fails: pass `ok(track->demand())`.

### Generated types drop the `Moq` prefix (#5727)

`moq::MoqFoo` is `moq::Foo`, the name the aliases already gave it.
`moq::expected` is `tl::expected` at every C++ standard instead of `std::expected` under C++23, and `MOQ_ABI`, `MOQ_ABI_NAME`, and `MOQ_IMPLEMENTATION` are gone.

### Media subscribe and fetch take a catalog rendition (#5834)

`moq::MediaTrackConfig` and `moq::MediaGroupConfig` replace `name` and `container` with a `moq::Rendition` and follow its broadcast and track themselves.

```cpp
auto media = ok(moq::MediaTrackConsumer::subscribe(broadcast, {moq::Rendition::kVideo{id, video}}).get());
```

### Native codecs have audio and video types (#5694)

`BroadcastProducer::encode_audio` / `encode_video` are `moq::AudioProducer::init` / `moq::VideoProducer::init`, which take a `moq::MediaCatalogProducer`.
The audio track name moves into `AudioEncoderOutput::track`, and a zero `frame_duration_us` takes the codec's own frame.
`BroadcastConsumer::decode_audio` / `decode_video` are `moq::AudioConsumer::subscribe` / `moq::VideoConsumer::subscribe`, taking the broadcast first.
Producers expose subscriber state only through `demand()`, and a video `gop` is a `moq::VideoGop` keyframe variant.
`moq::AudioEncoder` / `moq::VideoEncoder` encode without publishing.

```cpp
moq::AudioEncoderOutput output{moq::AudioCodec::opus()};
output.track = "audio";
auto catalog = ok(moq::MediaCatalogProducer::init(broadcast));
auto audio = ok(moq::AudioProducer::init(catalog, input, output, nullptr));
```
