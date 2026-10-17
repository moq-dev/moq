/// Catalogs, media imports, and container consumers.
library;

import 'package:moq_ffi/moq_ffi.dart';

/// The write side of a media track; discontinuity() marks a break between pre-framed payloads.
typedef TrackProducer = MoqMediaTrackProducer;

/// The write side of a media track fed a raw byte stream, with frame boundaries inferred.
typedef TrackStreamProducer = MoqMediaTrackStreamProducer;

/// The write side of a container, which publishes each track it describes.
typedef ContainerProducer = MoqMediaContainerProducer;

/// The write side of a container fed a raw byte stream.
typedef ContainerStreamProducer = MoqMediaContainerStreamProducer;

/// The read side of a media track: yields frames with codec metadata in decode order.
typedef TrackConsumer = MoqMediaTrackConsumer;

/// A finite fetched media group: yields container-decoded frames until the group ends.
typedef GroupConsumer = MoqMediaGroupConsumer;

/// The read side of a broadcast's catalog: yields updates as the set of tracks changes.
typedef CatalogConsumer = MoqMediaCatalogConsumer;

/// A broadcast's catalog: its tracks and their properties, plus any application sections.
typedef Catalog = MoqCatalog;

/// A media frame whose keyframe flag marks group starts or video keyframes; audio flags only group starts.
typedef MediaFrame = MoqMediaFrame;

/// The catalog description of a video track, including whether it is enabled (a disabled one has no frames coming).
typedef Video = MoqVideo;

/// Caller-provided catalog fields for a video track.
typedef VideoHint = MoqVideoHint;

/// A video codec, optional init bytes, a label, and catalog hints.
typedef VideoInit = MoqVideoInit;

/// Catalog properties shared by every video rendition; absent fields clear those properties.
typedef VideoProperties = MoqVideoProperties;

/// A single video codec an importer can parse.
typedef VideoFormat = MoqVideoFormat;

/// The catalog description of an audio track: codec, sample rate, channels, whether it is enabled, and container.
typedef Audio = MoqAudio;

/// An audio codec, its required init bytes, and an optional label.
typedef AudioInit = MoqAudioInit;

/// A single audio codec an importer can parse.
typedef AudioFormat = MoqAudioFormat;

/// A container that publishes its own tracks.
typedef ContainerFormat = MoqContainerFormat;

/// A shared source timestamp offset that does not keep its broadcast open.
typedef Timebase = MoqMediaTimebase;

/// A byte-stream container format and an optional shared source timebase.
typedef ContainerStreamInit = MoqContainerStreamInit;

/// A container format, its leading bytes, and an optional shared source timebase.
typedef ContainerInit = MoqContainerInit;

/// A width and height pair, in pixels.
typedef Dimensions = MoqDimensions;

/// A weak catalog handle that closes with its broadcast.
typedef CatalogProducer = MoqMediaCatalogProducer;

/// A named or requested track target for an importer.
typedef Target = MoqMediaTarget;

/// A new track with a chosen or format-derived name.
typedef Named = NamedMoqMediaTarget;

/// A subscriber-requested track.
typedef Requested = RequestedMoqMediaTarget;

/// A catalog rendition: its ID (catalog key) and its video or audio config.
typedef Rendition = MoqRendition;

/// A catalog video rendition.
typedef VideoRendition = VideoMoqRendition;

/// A catalog audio rendition.
typedef AudioRendition = AudioMoqRendition;

/// A catalog rendition and live delivery options.
typedef TrackConfig = MoqMediaTrackConfig;

/// A catalog rendition's group sequence and fetch delivery options.
typedef GroupConfig = MoqMediaGroupConfig;

/// The packaging advertised for a media track.
typedef Container = MoqContainer;

/// Duration view over a media frame's presentation time.
extension MediaFrameDuration on MoqMediaFrame {
  /// Presentation timestamp.
  Duration get timestamp => Duration(microseconds: timestampUs);
}
