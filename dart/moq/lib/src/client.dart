import 'package:moq_ffi/moq_ffi.dart';

import 'aliases.dart';

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

/// Everything [Moq.connect] can be told beyond the URL.
///
/// Every field is optional; a null keeps the native default. A value the native
/// side cannot use fails [Moq.connect] with `MoqException.Config`.
final class ConnectOptions {
  /// Set false to skip certificate verification (local dev only).
  final bool tlsVerify;

  /// PEM root certificate paths to trust instead of the platform roots.
  final List<String>? tlsRoots;

  /// Whether to also trust the platform roots when [tlsRoots] is set.
  final bool? tlsSystemRoots;

  /// Peer certificate SHA-256 fingerprints to pin.
  final List<String>? tlsFingerprints;

  /// Path to a PEM certificate chain to present for mTLS.
  final String? tlsCert;

  /// Path to a PEM private key to present for mTLS.
  final String? tlsKey;

  /// Local socket address to bind, e.g. `0.0.0.0:0`.
  final String? bind;

  /// Protocol versions to offer, most preferred first, e.g. `moq-lite-03`.
  /// Null offers every supported version.
  final List<String>? versions;

  /// Cap on the concurrent QUIC streams the peer may open toward this
  /// connection. MoQ opens one stream per group, so a subscriber to many
  /// tracks may want this raised.
  final int? maxStreams;

  /// Set false to stop the WebSocket fallback racing QUIC, e.g. against a
  /// relay that only serves QUIC. On by default.
  final bool? websocketEnabled;

  /// Head start QUIC gets before the WebSocket fallback joins the race; 200ms
  /// by default, and zero races both at once.
  final Duration? websocketDelay;

  /// Set false for a one-shot dial. By default the session redials with
  /// backoff whenever the transport drops.
  final bool reconnect;

  /// Retry pacing for the automatic reconnect.
  final Backoff? backoff;

  /// Origin to announce broadcasts through; auto-created when null.
  final OriginProducer? publish;

  /// Origin to discover broadcasts through; auto-created when null.
  final OriginProducer? consume;

  const ConnectOptions({
    this.tlsVerify = true,
    this.tlsRoots,
    this.tlsSystemRoots,
    this.tlsFingerprints,
    this.tlsCert,
    this.tlsKey,
    this.bind,
    this.versions,
    this.maxStreams,
    this.websocketEnabled,
    this.websocketDelay,
    this.reconnect = true,
    this.backoff,
    this.publish,
    this.consume,
  });

  MoqClientConfig get _ffi {
    final delay = websocketDelay;
    if (delay != null && delay.isNegative) {
      throw ArgumentError.value(
        delay,
        'websocketDelay',
        'must not be negative',
      );
    }
    return MoqClientConfig(
      bind: bind,
      versions: versions ?? const [],
      tls: MoqClientTls(
        insecure: !tlsVerify,
        roots: tlsRoots ?? const [],
        systemRoots: tlsSystemRoots,
        fingerprints: tlsFingerprints ?? const [],
        cert: tlsCert,
        key: tlsKey,
      ),
      quic: MoqQuicConfig(maxStreams: maxStreams),
      websocket: MoqWebSocketConfig(
        enabled: websocketEnabled,
        delayUs: delay?.inMicroseconds,
      ),
      once: !reconnect,
      backoff: backoff ?? MoqBackoff(),
      publish: publish,
      consume: consume,
    );
  }
}

/// A connected MoQ session with publishing and subscription conveniences.
final class Moq {
  final Client _client;

  /// The established raw MoQ session.
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

  /// Wait for a broadcast announced at exactly [path].
  AnnouncedBroadcast announcedBroadcast(String path) =>
      session.consume().announcedBroadcast(path: path);

  /// Resolve an existing broadcast at [path].
  Future<BroadcastConsumer> requestBroadcast(String path) =>
      session.consume().requestBroadcast(path: path);

  /// How many times this session has connected: 1 for the connect that built it, one more
  /// on each reconnect. A server-accepted session stays at 1.
  int get connects => session.connects();

  /// The session's bandwidth allocator.
  ///
  /// Every call returns a handle to the same registry. [Bandwidth.reserve]
  /// a share for an app-owned encoder; dropping the [Reservation] hands
  /// the room back.
  Bandwidth bandwidth() => session.bandwidth();

  /// Gracefully close the session and stop the client.
  Future<void> close() async {
    try {
      await session.shutdown();
    } finally {
      _client.cancel();
    }
  }
}

/// A [Stream] view over an announcement cursor.
extension AnnounceConsumerUpdates on AnnounceConsumer {
  /// Stream announce events until the cursor ends.
  ///
  /// Listen once: the cursor is cancelled and released when the subscription
  /// ends.
  Stream<AnnounceEvent> updates() async* {
    try {
      while (true) {
        final event = await next();
        if (event == null) return;
        yield event;
      }
    } finally {
      cancel();
      dispose();
    }
  }
}
