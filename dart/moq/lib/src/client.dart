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

/// A connected MoQ session with publishing and subscription conveniences.
final class Moq {
  final Client _client;

  /// The established raw MoQ session.
  final Session session;

  Moq._(this.session, this._client);

  /// Connect to a relay at [url].
  ///
  /// By default the session redials with backoff whenever the transport drops;
  /// pass `reconnect: false` for a one-shot dial, or a [backoff] to re-pace the
  /// retries.
  static Future<Moq> connect(
    String url, {
    bool tlsVerify = true,
    List<String>? tlsRoots,
    bool? tlsSystemRoots,
    List<String>? tlsFingerprints,
    String? tlsCert,
    String? tlsKey,
    String? bind,
    int? maxStreams,
    bool? reconnect,
    Backoff? backoff,
    OriginProducer? publish,
    OriginProducer? subscribe,
  }) async {
    final client = Client();
    try {
      if (!tlsVerify) client.setTlsVerify(verify: false);
      if (tlsRoots != null) client.setTlsRoots(paths: tlsRoots);
      if (tlsSystemRoots != null) {
        client.setTlsSystemRoots(systemRoots: tlsSystemRoots);
      }
      if (tlsFingerprints != null) {
        client.setTlsFingerprints(fingerprints: tlsFingerprints);
      }
      if (tlsCert != null) client.setTlsCert(path: tlsCert);
      if (tlsKey != null) client.setTlsKey(path: tlsKey);
      if (bind != null) client.setBind(addr: bind);
      if (maxStreams != null) client.setQuicMaxStreams(maxStreams: maxStreams);
      if (reconnect != null) client.setReconnect(enabled: reconnect);
      if (backoff != null) client.setBackoff(backoff: backoff);
      if (publish != null) client.setPublish(origin: publish);
      if (subscribe != null) client.setConsume(origin: subscribe);

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
