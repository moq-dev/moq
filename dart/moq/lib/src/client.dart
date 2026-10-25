import 'package:moq_ffi/moq_ffi.dart';

import 'aliases.dart';
import 'stream.dart';
import 'version.dart';

/// Scope for discovering announcements.
final class AnnounceOptions {
  /// Literal path root beneath the origin.
  final String prefix;

  /// Pattern relative to [prefix], or null for every path beneath it.
  final String? filter;

  /// Also list paths with a segment starting with `.` below [prefix].
  final bool hidden;

  const AnnounceOptions({this.prefix = '', this.filter, this.hidden = false});

  AnnounceConfig get _ffi =>
      AnnounceConfig(prefix: prefix, filter: filter, hidden: hidden);
}

/// Everything [Moq.connect] can be told beyond the URL, named and nested like
/// the FFI's [MoqClientConfig].
///
/// Every field is optional; a null keeps the native default. A value the native
/// side cannot use fails [Moq.connect] with `MoqException.Config`.
final class ConnectOptions {
  /// Local socket address to bind, e.g. `0.0.0.0:0`.
  final String? bind;

  /// Protocol versions to offer, most preferred first, e.g. `moq-lite-03`.
  /// Null offers every supported version, which [versions] lists.
  final List<String>? versions;

  /// How long one connect may take, dial and handshake together; zero waits forever.
  final Duration? timeout;

  /// Happy Eyeballs: how long before the next resolved address is also dialed.
  final Duration? race;

  /// Happy Eyeballs: how long IPv4 waits for the full DNS answer.
  final Duration? resolutionDelay;

  /// Certificate trust and the mTLS identity, such as `ClientTls(insecure: true)`
  /// for a self-signed relay during local development.
  final ClientTls? tls;

  /// QUIC transport tuning.
  final QuicConfig? quic;

  /// The WebSocket fallback raced against QUIC for `http(s)` URLs.
  final WebSocketConfig? websocket;

  /// Set true for a one-shot dial. By default the session redials with
  /// backoff whenever the transport drops.
  final bool once;

  /// Retry pacing for the automatic reconnect.
  final Backoff? backoff;

  /// Origin to announce broadcasts through; auto-created when null.
  final OriginProducer? publish;

  /// Origin to discover broadcasts through; auto-created when null.
  final OriginProducer? consume;

  const ConnectOptions({
    this.bind,
    this.versions,
    this.timeout,
    this.race,
    this.resolutionDelay,
    this.tls,
    this.quic,
    this.websocket,
    this.once = false,
    this.backoff,
    this.publish,
    this.consume,
  });

  MoqClientConfig get _ffi => MoqClientConfig(
    bind: bind,
    versions: versions ?? const [],
    timeout: timeout,
    race: race,
    resolutionDelay: resolutionDelay,
    tls: tls ?? ClientTls(),
    quic: (quic ?? const QuicConfig())._ffi,
    websocket: (websocket ?? const WebSocketConfig())._ffi,
    once: once,
    backoff: backoff ?? MoqBackoff(),
    implementation: implementation,
    publish: publish,
    consume: consume,
  );
}

/// QUIC transport tuning for [ConnectOptions.quic]; the WebSocket fallback
/// ignores it. Null fields keep the library or backend default.
final class QuicConfig {
  /// Cap on the concurrent QUIC streams the peer may open toward this
  /// connection. MoQ opens one stream per group, so a subscriber to many
  /// tracks may want this raised.
  final int? maxStreams;

  /// How long an idle QUIC connection survives before it is dropped.
  final Duration? idleTimeout;

  /// QUIC keep-alive ping interval; zero disables it.
  final Duration? keepAlive;

  /// UDP generic segmentation offload; null leaves the choice to the backend.
  final bool? gso;

  /// Path MTU discovery; null leaves the choice to the backend.
  final bool? mtuDiscovery;

  /// The congestion control family; null leaves the choice to the backend.
  final CongestionControl? congestionControl;

  /// Directory to write qlog traces into; fails unless [qlogSupported].
  final String? qlog;

  /// Danger: write TLS session secrets to the file `SSLKEYLOGFILE` names, for
  /// decrypting a capture. Off by default; on without `SSLKEYLOGFILE` fails.
  final bool keylog;

  const QuicConfig({
    this.maxStreams,
    this.idleTimeout,
    this.keepAlive,
    this.gso,
    this.mtuDiscovery,
    this.congestionControl,
    this.qlog,
    this.keylog = false,
  });

  MoqQuicConfig get _ffi => MoqQuicConfig(
    maxStreams: maxStreams,
    idleTimeout: idleTimeout,
    keepAlive: keepAlive,
    gso: gso,
    mtuDiscovery: mtuDiscovery,
    congestionControl: congestionControl,
    qlog: qlog,
    keylog: keylog,
  );
}

/// The WebSocket fallback for [ConnectOptions.websocket], raced against QUIC
/// for `http(s)` URLs.
final class WebSocketConfig {
  /// Set false to stop the fallback, e.g. against a relay that only serves
  /// QUIC. On by default.
  final bool? enabled;

  /// Head start QUIC gets before the fallback joins the race; 200ms by
  /// default, and zero races both at once.
  final Duration? delay;

  const WebSocketConfig({this.enabled, this.delay});

  MoqWebSocketConfig get _ffi =>
      MoqWebSocketConfig(enabled: enabled, delay: delay);
}

/// The protocol versions offered by default, most preferred first, spelled the
/// way [ConnectOptions.versions] takes them.
List<String> versions() => moqVersions();

/// Whether this build can write qlog traces for [QuicConfig.qlog].
bool qlogSupported() => moqQlogSupported();

/// A MoQ session with publishing and subscription conveniences.
final class Moq {
  final Client _client;

  /// The connecting or connected raw MoQ session.
  final Session session;

  Moq._(this.session, this._client);

  /// Connect to a relay at [url].
  ///
  /// With neither [ConnectOptions.publish] nor [ConnectOptions.consume]
  /// given, both sides of the session share one origin, so a broadcast
  /// announced here is discoverable through [announced]. Wiring either
  /// side opts out and isolates the two directions.
  static Future<Moq> connect(
    String url, {
    ConnectOptions options = const ConnectOptions(),
  }) async {
    final client = Client(config: options._ffi);
    try {
      final session = await client.connect(url: url);
      return Moq._(session, client);
    } catch (_) {
      client.cancel();
      rethrow;
    }
  }

  /// Create an unannounced broadcast at [path], invisible to everyone until announced.
  ///
  /// Advertise it with `announce` after populating tracks. Create, `dynamic`
  /// if tracks are served on demand, populate, then announce.
  BroadcastProducer createBroadcast(String path) =>
      session.publish().createBroadcast(path: path);

  /// Discover routes matching [options]; prefixes stay relative to the origin.
  ///
  /// Listen to `announced(...).updates()` for a [Stream] of [AnnounceEvent]
  /// that releases the cursor when the subscription ends.
  AnnounceConsumer announced({
    AnnounceOptions options = const AnnounceOptions(),
  }) => session.consume().announced(config: options._ffi);

  /// Follow the broadcast at exactly [path]: each `available()` resolves the
  /// next publisher instance serving it, such as a restarted publisher.
  AnnouncedBroadcast announcedBroadcast(String path) =>
      session.consume().announcedBroadcast(path: path);

  /// Resolve an existing broadcast at [path].
  Future<BroadcastConsumer> requestBroadcast(String path) =>
      session.consume().requestBroadcast(path: path);

  /// The session's bandwidth allocator.
  ///
  /// Every call returns a handle to the same registry. [Bandwidth.reserve]
  /// a share for an app-owned encoder; dropping the [Reservation] hands
  /// the room back.
  Bandwidth bandwidth() => session.bandwidth();

  /// The session's authorization: the grant its peer gave it, and more tokens to present.
  ///
  /// Listen to [AuthGrants.grants] to follow the grant, and [Auth.add] a fresh
  /// token before the current one expires; each added token is presented again
  /// on every reconnect.
  Auth auth() => session.auth();

  /// Drain within one second, then stop the client.
  ///
  /// Explicit `moq-lite-07-wip` waits for the peer to receive finished tails; older
  /// Lite versions, including the default lite-06, wait only for transport ACKs.
  /// IETF also waits for transport ACKs.
  Future<void> close() async {
    try {
      session.shutdown();
      await session.closed();
    } finally {
      _client.cancel();
    }
  }
}

/// A [Stream] view over a session's grant.
extension AuthGrants on Auth {
  /// The current grant, then each change, null while there is none.
  ///
  /// Listens on one [Auth] share its place in the changes, so a second watcher
  /// takes its own from [Moq.auth]. Cancelling releases only its pending read.
  /// Ends when the session shuts down, and errors with its close reason otherwise.
  Stream<Grant?> grants() => _Grants(this);
}

/// A [Stream] view over an announcement cursor.
extension AnnounceConsumerUpdates on AnnounceConsumer {
  /// Stream announce events until the cursor ends.
  ///
  /// Listen once: the cursor is cancelled and released when the subscription
  /// ends.
  Stream<AnnounceEvent> updates() => _Updates(this).cast<AnnounceEvent>();
}

final class _Grants extends NativeStream<Grant?> {
  final Auth auth;
  _Grants(this.auth);

  @override
  Iterable<Grant?> initial() => [auth.grant()];

  @override
  UniffiRustFuture<Grant?> read() => auth.grantChanged();

  @override
  bool ended(Object error) =>
      error is CancelledMoqException || error is ClosedMoqException;
}

final class _Updates extends NativeStream<AnnounceEvent?> {
  final AnnounceConsumer cursor;
  _Updates(this.cursor);

  @override
  UniffiRustFuture<AnnounceEvent?> read() => cursor.next();

  @override
  bool finished(AnnounceEvent? value) => value == null;

  @override
  void release() {
    try {
      cursor.cancel();
    } finally {
      cursor.dispose();
    }
  }
}
