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

/// The credentials a [Client] presents, replaceable while it runs.
typedef Tokens = MoqTokens;

/// A live pub/sub session with a relay, exposing publish and consume origins.
typedef Session = MoqSession;

/// An incoming session awaiting a decision: accept it to handshake, or reject it.
typedef Request = MoqRequest;

/// The network transport carrying an incoming session.
typedef Transport = MoqTransport;

/// The congestion control family for a QUIC connection.
typedef CongestionControl = MoqCongestionControl;

/// Certificate trust and the mTLS identity for `ConnectOptions.tls`.
typedef ClientTls = MoqClientTls;

/// The publish side of an origin: create broadcasts so subscribers can discover them.
typedef OriginProducer = MoqOriginProducer;

/// Config for creating an origin, such as its total cache budget.
typedef OriginConfig = MoqOriginConfig;

/// The subscribe side of an origin: discover and request published broadcasts.
typedef OriginConsumer = MoqOriginConsumer;

/// A served route: advertises a path prefix and yields broadcast requests beneath it.
typedef OriginRequests = MoqOriginRequests;

/// A requested broadcast not yet accepted: fulfill it with a producer or reject it.
typedef BroadcastRequest = MoqBroadcastRequest;

/// A claim answer being filled before its exact route is announced.
typedef BroadcastPrepared = MoqBroadcastPrepared;

/// A broadcast published on an origin: the broadcast to fill and its announcement.
typedef Publication = MoqPublication;

/// A stream of announce events under a prefix.
typedef AnnounceConsumer = MoqAnnounceConsumer;

/// A literal prefix, an optional relative pattern, and the hidden-path opt-in for announcement discovery.
typedef AnnounceConfig = MoqAnnounceConfig;

/// Follows a specific broadcast path across the publisher instances that serve it.
typedef Following = MoqFollowing;

/// How a broadcast advertises its exact path: its static production cost and publisher epoch.
typedef AnnounceRoute = MoqAnnounceRoute;

/// A route over a prefix: its origin-relative path, wildcard captures, and route metadata.
typedef Announce = MoqAnnounce;

/// What an [AnnounceConsumer] yields: [AnnounceEventStart],
/// [AnnounceEventUpdate], [AnnounceEventEnd], or [AnnounceEventRestart].
typedef AnnounceEvent = MoqAnnounceEvent;

/// A route now covers the prefix; the stream had none there.
typedef AnnounceEventStart = StartMoqAnnounceEvent;

/// The route covering the prefix changed hops or cost.
typedef AnnounceEventUpdate = UpdateMoqAnnounceEvent;

/// No route covers the prefix any more; carries its last route.
typedef AnnounceEventEnd = EndMoqAnnounceEvent;

/// Another publisher instance now serves the prefix: drop what was resolved
/// under it and request afresh.
typedef AnnounceEventRestart = RestartMoqAnnounceEvent;

/// The write side of a broadcast: publish tracks into it.
typedef BroadcastProducer = MoqBroadcastProducer;

/// The read side of a broadcast: subscribe to its catalog and tracks.
typedef BroadcastConsumer = MoqBroadcastConsumer;

/// Receives tracks requested from a dynamically served broadcast.
typedef BroadcastRequests = MoqBroadcastRequests;

/// The write side of a raw track: append groups of frames.
typedef TrackProducer = MoqTrackProducer;

/// A subscriber-requested track not yet accepted: accept it for a [TrackProducer] or abort it.
typedef TrackRequest = MoqTrackRequest;

/// A stream of the fills readers of one track need: runs of groups the cache lacks.
typedef TrackRequests = MoqTrackRequests;

/// A watch-only handle to whether a published track has subscribers; holding it keeps nothing open.
typedef TrackDemand = MoqTrackDemand;

/// The read side of a raw track: yields groups in sequence order, skipping ahead if it falls behind.
typedef TrackConsumer = MoqTrackConsumer;

/// A run of groups readers miss: accept each group you have and drop the rest.
typedef Fill = MoqFill;

/// The write side of a single group: append frames to it.
typedef GroupProducer = MoqGroupProducer;

/// The read side of a single group: yields timestamped raw frames.
typedef GroupConsumer = MoqGroupConsumer;

/// A datagram-delivered frame, tagged with a per-track sequence number.
typedef Datagram = MoqDatagram;

/// A payload plus the timestamp it should be presented at.
typedef Frame = MoqFrame;

/// Tunes how a track subscription is delivered: priority, latency budget, ranges, and group ordering.
typedef Subscription = MoqSubscription;

/// moq-net's defaults, whose `maxDelay` of zero skips a superseded group at once.
///
/// A null subscription means the same. A `Subscription()` leaves `maxDelay` null, which
/// sets no subscriber limit, so pass `maxDelay: Duration.zero` when building one to match.
Subscription defaultSubscription() => moqSubscriptionDefaults();

/// A span of a track to deliver: from `start` up to `end`, or onward when `end` is null.
typedef Range = MoqRange;

/// Where one end of a [Range] falls: absolute, relative to the latest group, or the group containing a time.
typedef Anchor = MoqAnchor;

/// Which pending group the publisher sends first.
typedef Order = MoqOrder;

/// Options for fetching one past group by sequence.
typedef FetchGroupOptions = MoqFetchGroupOptions;

/// Delivery settings for a raw track: priority, ordering, latency budget, and timescale.
typedef TrackInfo = MoqTrackInfo;

/// Divides one connection's send estimate among the tracks sharing it.
typedef Allocator = MoqAllocator;

/// An owned bandwidth estimate; release it to stop receive PROBE demand.
typedef BandwidthConsumer = MoqBandwidthConsumer;

/// One track's standing claim on an [Allocator].
typedef Reservation = MoqReservation;

/// A snapshot of transport connection statistics.
typedef ConnectionStats = MoqConnectionStats;

/// Follows the grant a session's peer gave it; cancel it to release a pending wait.
typedef GrantConsumer = MoqGrantConsumer;

/// What the peer lets this side do: publish and subscribe patterns, and an expiry.
typedef Grant = MoqGrant;

/// A connection lifecycle transition reported by [Session.state].
typedef ConnectionPhase = MoqConnectionPhase;

/// One coherent connection lifecycle snapshot.
typedef ConnectionState = MoqConnectionState;

/// Retry pacing for the automatic reconnect: initial delay, multiplier, ceiling, and give-up window.
typedef Backoff = MoqBackoff;

/// Whether a protocol code is from the session or stream registry.
typedef ErrorScope = MoqErrorScope;

/// A recognized protocol kind, or APP / UNKNOWN when the code is not named.
typedef ProtocolKind = MoqProtocolKind;

/// A protocol failure: scope, verbatim wire code, kind, and a diagnostic message.
typedef ProtocolException = MoqProtocolException;
