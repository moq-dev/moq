# Migrating the C++ moq library

How to update your code for each breaking change, newest first.

## Unreleased

### Bandwidth reserves a track's demand (#5845)

`Bandwidth::reserve` takes the track's `demand()` instead of the producer, and no longer fails.

```cpp
// Before
auto reservation = ok(bandwidth->reserve(track, max_bps));
// After
auto reservation = bandwidth->reserve(ok(track->demand()), max_bps);
```

### Generated types drop the `Moq` prefix (#5727)

`moq::MoqFoo` is `moq::Foo`, the name the aliases already gave it.
`moq::expected` is `tl::expected` at every C++ standard instead of `std::expected` under C++23, and `MOQ_ABI`, `MOQ_ABI_NAME`, and `MOQ_IMPLEMENTATION` are gone.

```cpp
// Before
moq::MoqClientConfig config;
// After
moq::ClientConfig config;
```

### Media subscribe and fetch take a catalog rendition (#5834)

`moq::MediaTrackConfig` and `moq::MediaGroupConfig` replace `name` and `container` with a `moq::Rendition` and follow its broadcast and track themselves.

```cpp
// Before
auto media = ok(moq::MediaTrackConsumer::subscribe(broadcast, {video.track.value_or(id), video.container}).get());
// After
auto media = ok(moq::MediaTrackConsumer::subscribe(broadcast, {moq::Rendition::kVideo{id, video}}).get());
```

### Native codecs have audio and video types (#5694)

`BroadcastProducer::encode_audio` / `encode_video` are `moq::AudioProducer::init` / `moq::VideoProducer::init`, which take a `moq::MediaCatalogProducer`.
The audio track name moves into `AudioEncoderOutput::track`, and a zero `frame_duration_us` takes the codec's own frame.
`BroadcastConsumer::decode_audio` / `decode_video` are `moq::AudioConsumer::subscribe` / `moq::VideoConsumer::subscribe`, taking the broadcast first.
Producers expose subscriber state only through `demand()`, and a video `gop` is a `moq::VideoGop` keyframe variant.
`moq::AudioEncoder` / `moq::VideoEncoder` encode without publishing.

```cpp
// Before
auto audio = ok(broadcast->encode_audio("audio", input, output, nullptr));
// After
moq::AudioEncoderOutput output{moq::AudioCodec::opus()};
output.track = "audio";
auto catalog = ok(moq::MediaCatalogProducer::init(broadcast));
auto audio = ok(moq::AudioProducer::init(catalog, input, output, nullptr));
```
