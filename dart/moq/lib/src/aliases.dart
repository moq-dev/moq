import 'package:moq_ffi/moq_ffi.dart';

// Re-export the UniFFI types without their `Moq` prefix, so consumers spell
// them the way the Rust API does. These are type aliases, not wrappers: the
// values are the exact same objects, so every generated method applies
// unchanged and the prefixed names stay valid. `Delay`, `Subscription`,
// `TrackInfo`, `TrackRefresh`, and `Timestamp` are extension types instead,
// which are the same objects too but add constructors: an omitted max delay or
// max age is the zero moq-net defaults to, where the generated record leaves it
// null (no limit), an omitted refresh asks for nothing, and a timestamp is built
// either scaled or [Timestamp.unscaled], as in Rust.
//
// Four types are deliberately not aliased, because the unprefixed name is
// already taken where this package is used:
//
//   * `MoqServer` - `Server` is the listen facade in server.dart.
//   * `MoqContainer` / `MoqRoute` - `Container` and `Route` are Flutter's, and
//     an ambiguous import would break every Flutter app that uses them. Its
//     options and facts are still [RouteOptions] and [RouteInfo].
//   * `MoqException` - `Exception` is in `dart:core`.

/// A MoQ client: configure the TLS/bind knobs, then connect to a relay.
typedef Client = MoqClient;

/// The credentials a [Client] presents, replaceable while it runs.
typedef Tokens = MoqTokens;

/// A request token a session presents, which a route or [Claim] names with
/// `setAuth`. Await `answered()` for the peer's verdict; `cancel()` withdraws it
/// unless a route or claim still names it.
typedef Credential = MoqCredential;

/// A live pub/sub session with a relay, exposing publish and consume origins.
typedef Session = MoqSession;

/// An incoming session awaiting a decision: accept it to handshake, or reject it.
typedef Request = MoqRequest;

/// The network transport carrying an incoming session.
typedef Transport = MoqTransport;

/// The congestion control algorithm for a QUIC connection. Build one with
/// `CongestionControl.bbr()`, `.cubic()`, or `.unlimited()`; a new algorithm joins
/// as a new constructor named after it.
typedef CongestionControl = MoqCongestionControl;

/// Certificate trust and the mTLS identity for `ConnectOptions.tls`.
typedef ClientTls = MoqClientTls;

/// The publish side of an origin: create routes and claims so subscribers can discover their broadcasts.
typedef OriginProducer = MoqOriginProducer;

/// A prefix route from `OriginProducer.claim`: a claim that every path beneath a prefix can be
/// served, yielding each request from `requested()`. Cancelling or disposing it retracts it and
/// rejects the requests still waiting.
typedef Claim = MoqClaim;

/// Config for creating an origin, such as its total cache budget.
typedef OriginConfig = MoqOriginConfig;

/// The subscribe side of an origin: discover and request published broadcasts.
typedef OriginConsumer = MoqOriginConsumer;

/// A requested broadcast not yet answered: serve a held broadcast, route a new one, or reject it.
typedef BroadcastRequest = MoqBroadcastRequest;

/// How a route advertises itself: hidden until announced, its static cost, and a stable [Identity].
typedef RouteOptions = MoqRouteOptions;

/// A stream of announce events under a prefix.
typedef AnnounceConsumer = MoqAnnounceConsumer;

/// The prefix to list, with every path beneath it, and the hidden-path opt-in for announcement discovery.
typedef AnnounceConfig = MoqAnnounceConfig;

/// Follows a specific broadcast path across the publisher identities that serve it.
typedef Following = MoqFollowing;

/// The read-only facts on a received route: a static cost, the stream's identity, and
/// whether a usable path reaches it.
typedef RouteInfo = MoqRouteInfo;

/// One continuous stream at a path: `Identity.parse` names one, and only a session mints one.
///
/// Two are equal only when their text and minted flag both match, so pass an
/// identity a route reported back as is, rather than re-parsing its text.
typedef Identity = MoqIdentity;

/// A route over a prefix: its origin-relative path and route metadata.
typedef Announce = MoqAnnounce;

/// What an [AnnounceConsumer] yields: [AnnounceEventPresent] or
/// [AnnounceEventAbsent].
///
/// Another publisher identity taking a path over keeps it present: hold a
/// [Following] to see the broadcast change.
typedef AnnounceEvent = MoqAnnounceEvent;

/// A route now covers the path, which was absent; it may arrive unavailable.
typedef AnnounceEventPresent = PresentMoqAnnounceEvent;

/// No route covers the path any more, whether its origin withdrew it or this
/// side let go of it; carries its last route.
typedef AnnounceEventAbsent = AbsentMoqAnnounceEvent;

/// The write side of a broadcast: publish tracks into it.
typedef BroadcastProducer = MoqBroadcastProducer;

/// The read side of a broadcast: subscribe to its catalog and tracks.
typedef BroadcastConsumer = MoqBroadcastConsumer;

/// Receives tracks requested from a dynamically served broadcast.
typedef BroadcastRequests = MoqBroadcastRequests;

/// The write side of a raw track: append groups of frames.
typedef TrackProducer = MoqTrackProducer;

/// A subscriber-requested track not yet accepted: accept it for a [TrackProducer] or reject it.
typedef TrackRequest = MoqTrackRequest;

/// A stream of the fills readers of one track need: runs of groups the cache lacks.
typedef TrackRequests = MoqTrackRequests;

/// A watch-only handle to whether a published track has subscribers; holding it keeps nothing open.
typedef TrackDemand = MoqTrackDemand;

/// The read side of a raw track: yields groups in start order, skipping ahead if it falls behind.
typedef TrackConsumer = MoqTrackConsumer;

/// A run of groups readers miss: accept each group you have and drop the rest.
typedef Fill = MoqFill;

/// The write side of a single group: append frames to it.
typedef GroupProducer = MoqGroupProducer;

/// The read side of a single group: yields timestamped raw frames.
typedef GroupConsumer = MoqGroupConsumer;

/// The read side of a metadata subscription: yields a track's groups in arrival order, each frame as its header alone.
typedef MetaConsumer = MoqMetaConsumer;

/// One group of a metadata subscription: yields its frames' headers, without their payloads.
typedef MetaGroup = MoqMetaGroup;

/// A datagram payload with its own sequence and timestamp.
typedef Datagram = MoqDatagram;

/// A payload plus the timestamp it should be presented at.
typedef Frame = MoqFrame;

/// A frame's header without its payload: its declared size in bytes (0 for an empty frame) and timestamp.
typedef FrameHeader = MoqFrameHeader;

/// An exact media time: `value` units at `scale` units per second.
///
/// A scale-less track has no time, so there `scale` is null and `value` is the
/// frame's group key (a datagram's sequence), as [Timestamp.unscaled] builds.
/// Reads return the generated [MoqTimestamp], which this type implements.
extension type Timestamp._(MoqTimestamp _timestamp) implements MoqTimestamp {
  /// `value` units at `scale` units per second.
  Timestamp({required int value, required int scale})
    : this._(MoqTimestamp(value: value, scale: scale));

  /// The scale-less timestamp at group key `key`, or a datagram's sequence.
  Timestamp.unscaled(int key) : this._(MoqTimestamp(value: key, scale: null));
}

/// How far behind the live edge a subscriber or decoder plays: where it starts, and how late it may fall.
extension type Delay._(MoqDelay _delay) implements MoqDelay {
  /// `min` is the target latency, and `max` is how far a non-latest group may fall behind before
  /// it is skipped, and how far newer media may run past a missing group before a decoder gives
  /// it up.
  /// Once `min` is stated, zero included, a [Range] with no start is a live join that far behind
  /// the live edge. Omitted or null, it is unstated: such a range reaches back as far as `max`
  /// allows, and a decoder starts it as zero. `max` defaults to zero, moq-net's default, and an
  /// explicit null sets no subscriber limit. A `min` past `max` starts a live join no further
  /// back than the group covering the point `max` behind the live edge.
  Delay({Duration? min, Duration? max = Duration.zero})
    : this._(MoqDelay(min: min, max: max));
}

/// Tunes how a track subscription is delivered: delay, and ranges, each with its own priority
/// and group ordering.
extension type Subscription._(MoqSubscription _subscription)
    implements MoqSubscription {
  /// An omitted `delay` is a bare [Delay], so a bare `Subscription()` is moq-net's defaults.
  Subscription({MoqDelay? delay, List<MoqRange>? ranges})
    : this._(MoqSubscription(delay: delay ?? Delay(), ranges: ranges));
}

/// moq-net's defaults, whose `delay` (no `min`, a zero `max`) starts at the newest group and skips a superseded one at once.
///
/// A null subscription and a bare `Subscription()` mean the same.
Subscription defaultSubscription() => Subscription._(moqSubscriptionDefaults());

/// A span of a track to deliver: from the group containing `start` through the group containing
/// `end`, each a [Timestamp.unscaled] time in the track's timescale (a scaled one is refused).
/// Every end includes its whole group. A null `start` is no start, which the subscription's
/// [Delay] places: a stated `min` is a live join that far behind the live edge (zero is the
/// latest group), and an unstated one reaches back as far as `max` allows. A null `end` keeps
/// following the live edge. `priority` is how urgently its groups go out, higher first, and
/// `order` which of them comes first, null for newest first. Before moq-lite-07 the publisher
/// sends every range at the highest priority any asks, newest first.
typedef Range = MoqRange;

/// Which of a range's groups comes first, as the publisher sends them and `recvGroup` returns
/// those held. Before moq-lite-07 the publisher sends newest first whatever this asks.
typedef Order = MoqOrder;

/// A location within a track: a group start and a frame index in it.
typedef Position = MoqPosition;

/// Options for fetching one past group by start.
typedef FetchGroupOptions = MoqFetchGroupOptions;

/// How a track's frames are compressed on the wire: `Compression.deflate()`, or null in
/// [TrackInfo] for none. Compare a received one by `name()`.
typedef Compression = MoqCompression;

/// When a subscriber asks a track's publisher for a new group, which
/// `TrackDemand.refresh()` reports.
extension type TrackRefresh._(MoqTrackRefresh _refresh)
    implements MoqTrackRefresh {
  /// A join asks once the newest group outweighs `ratio` times its first frame
  /// (any join when null), waiting out `interval` (null for no wait), which caps
  /// how often the publisher makes groups however many subscribers ask. A bare
  /// `TrackRefresh()` asks for nothing.
  TrackRefresh({int? ratio, Duration? interval})
    : this._(MoqTrackRefresh(ratio: ratio, interval: interval));
}

/// Delivery settings for a raw track: priority, max age, refresh, timescale, and compression.
extension type TrackInfo._(MoqTrackInfo _info) implements MoqTrackInfo {
  /// `maxAge` is how long a superseded group stays fresh, defaulting to zero, moq-net's
  /// default, which serves only the live edge. An explicit null sets no publisher limit.
  /// An omitted `refresh` asks for nothing.
  TrackInfo({
    int priority = 0,
    Duration? maxAge = Duration.zero,
    MoqTrackRefresh? refresh,
    int? timescale,
    MoqCompression? compression,
  }) : this._(
         MoqTrackInfo(
           priority: priority,
           maxAge: maxAge,
           refresh: refresh ?? TrackRefresh(),
           timescale: timescale,
           compression: compression,
         ),
       );
}

/// Divides one connection's send estimate among the tracks sharing it.
typedef Allocator = MoqAllocator;

/// An owned bandwidth estimate; release it to stop receive PROBE demand.
typedef BandwidthConsumer = MoqBandwidthConsumer;

/// One track's standing claim on an [Allocator].
typedef Reservation = MoqReservation;

/// Divides one connection's receive estimate among the broadcasts watched over it.
typedef Viewers = MoqViewers;

/// One watched broadcast's standing claim on [Viewers]; holding it requests
/// PROBE, disposing it releases the claim.
typedef Viewer = MoqViewer;

/// Turns a [Reservation]'s grant into a sender's [RateTarget]; call `update` on every frame.
typedef RateControl = MoqRateControl;

/// How a [RateControl] maps a grant onto a bitrate, and the lower rendition that disables it.
typedef RatePolicy = MoqRatePolicy;

/// What a sender following a [RateControl] should be doing: an enabled bitrate, or disabled.
typedef RateTarget = MoqRateTarget;

/// A snapshot of transport connection statistics.
typedef ConnectionStats = MoqConnectionStats;

/// Follows the grant a session's peer gave it; cancel it to release a pending wait.
typedef GrantConsumer = MoqGrantConsumer;

/// What the peer lets this side do: publish and subscribe patterns, and an expiry.
typedef Grant = MoqGrant;

/// Where a connection is in its lifecycle, carrying that phase's failure or version.
typedef ConnectionPhase = MoqConnectionPhase;

/// No session has connected yet; carries the latest failed dial.
typedef ConnectionPhaseConnecting = ConnectingMoqConnectionPhase;

/// A session is serving; carries its negotiated version.
typedef ConnectionPhaseConnected = ConnectedMoqConnectionPhase;

/// No session is serving while the connection redials; carries why.
typedef ConnectionPhaseDisconnected = DisconnectedMoqConnectionPhase;

/// The old session serves while its replacement dials; carries its version and the replacement's failure.
typedef ConnectionPhaseMigrating = MigratingMoqConnectionPhase;

/// The connection has stopped for good; carries the terminal error, if any.
typedef ConnectionPhaseClosed = ClosedMoqConnectionPhase;

/// One coherent connection lifecycle snapshot.
typedef ConnectionState = MoqConnectionState;

/// Retry pacing for the automatic reconnect: initial delay, multiplier, ceiling, and give-up window.
/// A null field keeps its default, zero means zero (retry at once, or give up
/// at the first failure), and `maxUnlimited` lifts the ceiling. Until the peer
/// has accepted a session's credential or drained a session with a GOAWAY
/// (or, on moq-lite 03/04, which never answer it, a session stayed up 1s), a failed dial or a closed
/// session, such as a refused credential, ends the connection with its error
/// rather than retrying.
typedef Backoff = MoqBackoff;

/// Whether a protocol code is from the session or stream registry.
typedef ErrorScope = MoqErrorScope;

/// A recognized protocol kind, or APP / UNKNOWN when the code is not named.
typedef ProtocolKind = MoqProtocolKind;

/// A protocol failure: scope, verbatim wire code, kind, and a diagnostic message.
typedef ProtocolException = MoqProtocolException;

/// Writes live datagrams with an independent sequence counter and no cache.
typedef DatagramProducer = MoqDatagramProducer;

/// Reads a live datagram track in arrival order from its own cursor.
typedef DatagramConsumer = MoqDatagramConsumer;

/// A requested datagram track awaiting acceptance or rejection.
typedef DatagramRequest = MoqDatagramRequest;

/// Serves on-demand datagram tracks through their own request queue.
typedef DatagramRequests = MoqDatagramRequests;

/// Publisher priority and optional timestamp scale fixed for a datagram track.
typedef DatagramInfo = MoqDatagramInfo;
