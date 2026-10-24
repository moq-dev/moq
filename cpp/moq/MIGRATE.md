# Migrating the C++ moq library

How to update your code for each breaking change, newest first.

## Unreleased

### Exact encoder rates and publication options

Raw video input framerates are exact numerator/denominator records, not integers.
Use `VideoFrameRate{30000, 1001}` and construct producers with `(catalog, AudioProducerOptions{input, output, track, bandwidth})` or the corresponding `VideoProducerOptions`.
The codec-only output records no longer contain track naming. Rebuild against
the matching generated bindings and native library.

### Video rendition preference

`moq::Video` gains `int32_t preference`. Update aggregate initializers with `0` to keep the default ranking and rebuild against the matching generated headers and native library. Higher values are preferred; compatibility transcodes use `-1`.

### `announce` takes an `AnnounceRoute`

`BroadcastProducer::announce` takes `moq::AnnounceRoute{cost, epoch}` instead of a `moq::Route`: the origin builds the hop chain, so `hops` is no longer an input.
`moq::Route` is only what announcements report. To re-announce an observed route, copy its `cost` and `epoch`.

### A broadcast reference waits for its sibling's announcement (#6254)

Resolving a rendition's `broadcast` reference, through `BroadcastConsumer::resolve` and the rendition-based subscribes and fetches, now waits for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, bound the call yourself: cancel it.

### Priorities are signed (#6142)

`TrackInfo::priority`, `Subscription::priority`, and `FetchGroupOptions::priority` are `int8_t`, and `GroupRequest::priority()` returns one.
Higher is first and 0 is the unset midpoint; `TrackInfo::priority` defaulted to 127 and now defaults to 0.
To keep a value's moq-lite byte, subtract 128 from it, widening first (`static_cast<int8_t>(int(old) - 128)`): 255 becomes 127, 128 becomes 0, and 0 becomes -128.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `announced` or `announced_broadcast`, instead of under its whole scope at connect.
`OriginConsumer::request_broadcast` watches nothing, so on a client it fails with an unroutable error for a path nothing watches, even when the relay announces it.
Resolve the path with `announced_broadcast`, or hold `announced` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.

### `moq::ContainerFormat::kFmp4` unwraps to Legacy (#6250)

A container import with `moq::ContainerFormat::kFmp4` now publishes one Legacy frame per sample instead of one CMAF frame per fragment.
Pass the new `moq::ContainerFormat::kCmaf` to keep publishing CMAF.

### Catalog references carry their epoch

`Video` and `Audio` gain `epoch`, the publisher instance their `broadcast` reference names, right after `broadcast`; positional initializers shift by one.
`BroadcastConsumer::resolve(reference)` becomes `resolve(rendition)`, taking the catalog `Rendition` and reading its `broadcast` and `epoch` from it.
Resolving, and the rendition-based consumers, fail with `moq::Error::kMux` for a rendition whose broadcast is proven to serve another epoch.

### Timestamps stop at 2^61 - 1 microseconds (#6168)

Writing a frame, or any other call that takes a timestamp, fails with `TimeOverflow` past `2^61 - 1` microseconds instead of `2^62 - 1`, the most moq-lite-05/06 can carry.
A received timestamp past it fails the group as a malformed track.
Clamp timestamps you build from unbounded input to `2^61 - 1`.

### Finished tracks stay until the broadcast ends (#6157)

A finished track now stays listed, with its catalog rendition and demand, until its broadcast closes or the new `BroadcastProducer::finish` lets go.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.

### A claim answers by publishing (#5678)

`OriginProducer::dynamic(prefix)` takes no route, and `OriginDynamic::update` is gone.
Replace `request->accept(broadcast)` with `request->prepare(epoch)`, which returns a `BroadcastPrepared`: fill its `broadcast()`, then `publish()` announces it at the exact path under that epoch and returns its producer.

### Anchors can name a time (#6074)

`Anchor` gains a `Time` case holding `timestamp_us`, the group containing that presentation time, so exhaustive visits need a case for it.
`TrackConsumer::update` now returns a result, failing with `TimestampMismatch` on a time anchor an untimed track can't serve.

### Rebuild the matching raw bindings

The documentation update changes UniFFI's audio-producer method metadata checksums.
Regenerate the C++ binding headers and sources from the rebuilt native library,
then rebuild the application; older generated guards reject the rebuilt library.
Application calls are unchanged.

### JSON consumers return `JsonValue` (#5677)

`JsonSnapshotConsumer::next()` and `JsonStreamConsumer::next()` return `JsonValue` (`value`, `timestamp_us`) instead of a string; `timestamp_us` is empty when untimed.
A snapshot consumer's `next()` now yields every state in order; call `latest()` for the newest.
Timestamps are rounded down to a whole microsecond.

### Ranges filter reads (#5929)

A subscription now reads only the groups its own `ranges` cover.

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

The boolean opt-in remains unchanged. Native QUIC captures `SSLKEYLOGFILE` when
configuration is adapted, creates new Unix logs with mode 0600, and refuses an
existing group/other-accessible file (for example mode 0644) without changing it.
Make an existing log private before enabling key logging.

### Subscriptions carry group ranges (#5732)

`Subscription::group_start` and `group_end` are replaced by `ranges`, a list of `Range` whose ends are an `Anchor` (an absolute group and frame, or a count of groups before the latest), and an optional `order`.
To start at group `n`, pass one range starting at the absolute anchor for group `n`, frame 0.

### Receive estimates come from `Session::recv_bandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `Session::recv_bandwidth` returns to request PROBE, read it with `rate()` or wait with `rate_changed()`, and `cancel()` it to stop.

```cpp
auto bandwidth = session->recv_bandwidth();
auto rate = bandwidth ? bandwidth->rate() : std::nullopt;
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
The audio track name belongs to `AudioProducerOptions::track`, and a zero `frame_duration_us` takes the codec's own frame.
`BroadcastConsumer::decode_audio` / `decode_video` are `moq::AudioConsumer::subscribe` / `moq::VideoConsumer::subscribe`, taking the broadcast first.
Producers expose subscriber state only through `demand()`, and a video `gop` is a `moq::VideoGop` keyframe variant.
`moq::AudioEncoder` / `moq::VideoEncoder` encode without publishing.

```cpp
moq::AudioEncoderOutput output{moq::AudioCodec::opus()};
auto catalog = ok(moq::MediaCatalogProducer::init(broadcast));
auto audio = ok(moq::AudioProducer::init(catalog, {input, output, "audio", nullptr}));
```
