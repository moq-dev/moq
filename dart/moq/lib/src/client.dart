import 'aliases.dart';

/// Scope for discovering announcements.
final class AnnounceOptions {
  /// Literal path root beneath the origin.
  final String prefix;

  /// Pattern relative to [prefix], or null for every path beneath it.
  final String? filter;

  const AnnounceOptions({this.prefix = '', this.filter});

  AnnounceConfig get _ffi => AnnounceConfig(prefix: prefix, filter: filter);
}

/// Everything [Moq.connect] can be told beyond the URL.
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

  /// Cap on the concurrent QUIC streams the peer may open toward this
  /// connection. MoQ opens one stream per group, so a subscriber to many
  /// tracks may want this raised.
  final int? maxStreams;

  /// Set false for a one-shot dial. By default the session redials with
  /// backoff whenever the transport drops.
  final bool? reconnect;

  /// Retry pacing for the automatic reconnect.
  final Backoff? backoff;

  /// Origin to announce broadcasts through; auto-created when null.
  final OriginProducer? publish;

  /// Origin to discover broadcasts through; auto-created when null.
  final OriginProducer? subscribe;

  const ConnectOptions({
    this.tlsVerify = true,
    this.tlsRoots,
    this.tlsSystemRoots,
    this.tlsFingerprints,
    this.tlsCert,
    this.tlsKey,
    this.bind,
    this.maxStreams,
    this.reconnect,
    this.backoff,
    this.publish,
    this.subscribe,
  });
}

/// A connected MoQ session with publishing and subscription conveniences.
final class Moq {
  final Client _client;

  /// The established raw MoQ session.
  final Session session;

  Moq._(this.session, this._client);

  /// Connect to a relay at [url].
  ///
  /// With neither [ConnectOptions.publish] nor [ConnectOptions.subscribe]
  /// given, both sides of the session share one origin, so a broadcast
  /// announced here is discoverable through [announcements]. Wiring either
  /// side opts out and isolates the two directions.
  static Future<Moq> connect(
    String url, {
    ConnectOptions options = const ConnectOptions(),
  }) async {
    final client = Client();
    try {
      if (!options.tlsVerify) client.setTlsVerify(verify: false);
      if (options.tlsRoots != null) {
        client.setTlsRoots(paths: options.tlsRoots!);
      }
      if (options.tlsSystemRoots != null) {
        client.setTlsSystemRoots(systemRoots: options.tlsSystemRoots!);
      }
      if (options.tlsFingerprints != null) {
        client.setTlsFingerprints(fingerprints: options.tlsFingerprints!);
      }
      if (options.tlsCert != null) client.setTlsCert(path: options.tlsCert);
      if (options.tlsKey != null) client.setTlsKey(path: options.tlsKey);
      if (options.bind != null) client.setBind(addr: options.bind!);
      if (options.maxStreams != null) {
        client.setQuicMaxStreams(maxStreams: options.maxStreams!);
      }
      if (options.reconnect != null) {
        client.setReconnect(enabled: options.reconnect!);
      }
      if (options.backoff != null) {
        client.setBackoff(backoff: options.backoff!);
      }
      if (options.publish != null) client.setPublish(origin: options.publish);
      if (options.subscribe != null) {
        client.setConsume(origin: options.subscribe);
      }

      final session = await client.connect(url: url);
      return Moq._(session, client);
    } catch (_) {
      client.cancel();
      rethrow;
    }
  }

  /// Create an unadvertised broadcast at [path].
  ///
  /// Advertise it with `announce` after populating tracks. Create, `dynamic`
  /// if tracks are served on demand, populate, then announce.
  BroadcastProducer createBroadcast(String path) =>
      session.publish().createBroadcast(path: path);

  /// Stream routes matching [options]; update prefixes stay relative to the origin.
  Stream<AnnounceUpdate> announcements({
    AnnounceOptions options = const AnnounceOptions(),
  }) async* {
    final announced = session.consume().announced(config: options._ffi);
    try {
      while (true) {
        final announcement = await announced.next();
        if (announcement == null) return;
        yield announcement;
      }
    } finally {
      announced.cancel();
      announced.dispose();
    }
  }

  /// Return the raw cursor for [options].
  AnnounceConsumer announced({
    AnnounceOptions options = const AnnounceOptions(),
  }) => session.consume().announced(config: options._ffi);

  /// Wait for a broadcast announced at exactly [path].
  AnnouncedBroadcast announcedBroadcast(String path) =>
      session.consume().announcedBroadcast(path: path);

  /// Resolve an existing broadcast at [path].
  Future<BroadcastConsumer> requestBroadcast(String path) =>
      session.consume().requestBroadcast(path: path);

  /// The connection epoch: 1 for the connect that built this session, one more
  /// on each reconnect. A server-accepted session stays at 1.
  int get epoch => session.epoch();

  /// The session's bandwidth allocator.
  ///
  /// Every call returns a handle to the same registry. [Bandwidth.reserve]
  /// a share for an app-owned encoder; dropping the [Reservation] hands
  /// the room back.
  Bandwidth bandwidth() => session.bandwidth();

  /// Gracefully close the session and stop the client.
  void close() {
    session.shutdown();
    _client.cancel();
  }
}
