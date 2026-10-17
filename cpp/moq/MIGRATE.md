# Migrating the C++ moq library

How to update your code for each breaking change, newest first.

## Unreleased

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
