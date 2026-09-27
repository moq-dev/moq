import 'package:moq_ffi/moq_ffi.dart';

// Re-export the UniFFI types without their `Moq` prefix, so consumers spell
// them the way the Rust API does. These are type aliases, not wrappers: the
// values are the exact same objects, so every generated method applies
// unchanged and the prefixed names stay valid.
//
// Four types are deliberately not aliased, because the unprefixed name is
// already taken where this package is used:
//
//   * `MoqServer` - `Server` is the listen facade in server.dart.
//   * `MoqContainer` / `MoqRoute` - `Container` and `Route` are Flutter's, and
//     an ambiguous import would break every Flutter app that uses them.
//   * `MoqException` - `Exception` is in `dart:core`.

/// A MoQ client: configure the TLS/bind knobs, then connect to a relay.
typedef Client = MoqClient;

/// A live pub/sub session with a relay, exposing publish and consume origins.
typedef Session = MoqSession;

/// An incoming session awaiting a decision: accept it to handshake, or reject it.
typedef Request = MoqRequest;

/// The network transport carrying an incoming session.
typedef Transport = MoqTransport;

/// The publish side of an origin: create broadcasts so subscribers can discover them.
typedef OriginProducer = MoqOriginProducer;

/// Config for creating an origin, such as its total cache budget.
typedef OriginConfig = MoqOriginConfig;

/// The subscribe side of an origin: discover and request published broadcasts.
typedef OriginConsumer = MoqOriginConsumer;

/// A served route: advertises a path prefix and yields broadcast requests beneath it.
typedef OriginDynamic = MoqOriginDynamic;

/// A requested broadcast not yet accepted: fulfill it with a producer or reject it.
typedef BroadcastRequest = MoqBroadcastRequest;

/// A stream of route announcements and retractions under a prefix.
typedef AnnounceConsumer = MoqAnnounceConsumer;

/// A literal prefix, an optional relative pattern, and the hidden-path opt-in for announcement discovery.
typedef AnnounceConfig = MoqAnnounceConfig;

/// A pending wait for a route to cover a specific path.
typedef AnnouncedBroadcast = MoqAnnouncedBroadcast;

/// A single route announcement or retraction: its path, route metadata, and active flag.
typedef AnnounceUpdate = MoqAnnounceUpdate;

/// The write side of a broadcast: publish tracks into it.
typedef BroadcastProducer = MoqBroadcastProducer;

/// The read side of a broadcast: subscribe to its catalog and tracks.
typedef BroadcastConsumer = MoqBroadcastConsumer;

/// Receives tracks requested from a dynamically served broadcast.
typedef BroadcastDynamic = MoqBroadcastDynamic;

/// The write side of a raw track: append groups of frames.
typedef TrackProducer = MoqTrackProducer;

/// A subscriber-requested track not yet accepted: accept it for a [TrackProducer] or abort it.
typedef TrackRequest = MoqTrackRequest;

/// A stream of uncached group requests for one track, for serving fetches on demand.
typedef TrackDynamic = MoqTrackDynamic;

/// A watch-only handle to whether a published track has subscribers; holding it keeps nothing open.
typedef TrackDemand = MoqTrackDemand;

/// The read side of a raw track: yields groups in sequence order, skipping ahead if it falls behind.
typedef TrackConsumer = MoqTrackConsumer;

/// A request to produce one uncached group for a fetch consumer.
typedef GroupRequest = MoqGroupRequest;

/// The write side of a single group: append frames to it.
typedef GroupProducer = MoqGroupProducer;

/// The read side of a single group: yields timestamped raw frames.
typedef GroupConsumer = MoqGroupConsumer;

/// The write side of a media track; discontinuity() marks a break between pre-framed payloads.
typedef MediaProducer = MoqMediaProducer;

/// The write side of a media track fed a raw byte stream, with frame boundaries inferred.
typedef MediaStreamProducer = MoqMediaStreamProducer;

/// The write side of a container, which publishes each track it describes.
typedef ContainerProducer = MoqContainerProducer;

/// The write side of a container fed a raw byte stream.
typedef ContainerStreamProducer = MoqContainerStreamProducer;

/// The read side of a media track: yields frames with codec metadata in decode order.
typedef MediaConsumer = MoqMediaConsumer;

/// A finite fetched media group: yields container-decoded frames until the group ends.
typedef MediaGroupConsumer = MoqMediaGroupConsumer;

/// The read side of a broadcast's catalog: yields updates as the set of tracks changes.
typedef CatalogConsumer = MoqCatalogConsumer;

/// Publishes lossy latest-value JSON snapshots.
typedef JsonSnapshotProducer = MoqJsonSnapshotProducer;

/// Consumes reconstructed latest-value JSON snapshots.
typedef JsonSnapshotConsumer = MoqJsonSnapshotConsumer;

/// Configures a lossy latest-value JSON track.
typedef JsonSnapshotConfig = MoqJsonSnapshotConfig;

/// Publishes a lossless stream of JSON records.
typedef JsonStreamProducer = MoqJsonStreamProducer;

/// Consumes a lossless stream of JSON records.
typedef JsonStreamConsumer = MoqJsonStreamConsumer;

/// Configures a lossless JSON stream track.
typedef JsonStreamConfig = MoqJsonStreamConfig;

/// A broadcast's catalog: its tracks and their properties, plus any application sections.
typedef Catalog = MoqCatalog;

/// A datagram-delivered frame, tagged with a per-track sequence number.
typedef Datagram = MoqDatagram;

/// A payload plus the timestamp it should be presented at.
typedef Frame = MoqFrame;

/// A media [Frame] whose keyframe flag marks group starts or video keyframes; audio flags only group starts.
typedef MediaFrame = MoqMediaFrame;

/// The catalog description of a video track, including whether the publisher recommends temporarily avoiding it.
typedef Video = MoqVideo;

/// Caller-provided catalog fields for a video track.
typedef VideoHint = MoqVideoHint;

/// A video codec, optional init bytes, a label, and catalog hints.
typedef VideoInit = MoqVideoInit;

/// Catalog properties shared by every video rendition; absent fields clear those properties.
typedef VideoProperties = MoqVideoProperties;

/// A single video codec an importer can parse.
typedef VideoFormat = MoqVideoFormat;

/// The catalog description of an audio track: codec, sample rate, channels, and container.
typedef Audio = MoqAudio;

/// An audio codec, its required init bytes, and an optional label.
typedef AudioInit = MoqAudioInit;

/// A single audio codec an importer can parse.
typedef AudioFormat = MoqAudioFormat;

/// A container that publishes its own tracks.
typedef ContainerFormat = MoqContainerFormat;

/// A container format and its leading bytes.
typedef ContainerInit = MoqContainerInit;

/// A width and height pair, in pixels.
typedef Dimensions = MoqDimensions;

/// Tunes how a track subscription is delivered: priority, group ordering, and range.
typedef Subscription = MoqSubscription;

/// Options for fetching one past group by sequence.
typedef FetchGroupOptions = MoqFetchGroupOptions;

/// Delivery settings for a raw track: priority, ordering, latency budget, and timescale.
typedef TrackInfo = MoqTrackInfo;

/// Divides one connection's send estimate among the tracks sharing it.
typedef Bandwidth = MoqBandwidth;

/// One track's standing claim on a [Bandwidth].
typedef Reservation = MoqReservation;

/// A snapshot of transport connection statistics.
typedef ConnectionStats = MoqConnectionStats;

/// A connection lifecycle transition reported by [Session.status].
typedef ConnectionStatus = MoqConnectionStatus;

/// Retry pacing for the automatic reconnect: initial delay, multiplier, ceiling, and give-up window.
typedef Backoff = MoqBackoff;

/// Whether a protocol code is from the session or stream registry.
typedef ErrorScope = MoqErrorScope;

/// A recognized protocol kind, or APP / UNKNOWN when the code is not named.
typedef ProtocolKind = MoqProtocolKind;

/// A protocol failure: scope, verbatim wire code, kind, and a diagnostic message.
typedef ProtocolException = MoqProtocolException;
