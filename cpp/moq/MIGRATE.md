# Migrating the C++ moq library

How to update your code for each breaking change, newest first.

## Unreleased

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
