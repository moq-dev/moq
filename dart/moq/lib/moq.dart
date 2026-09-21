/// Idiomatic Dart and Flutter client for Media over QUIC.
library;

import 'package:moq_ffi/moq_ffi.dart';

export 'package:moq_ffi/moq_ffi.dart';

/// Scope for discovering announcements.
final class AnnounceOptions {
  /// Literal path root beneath the origin.
  final String prefix;

  /// Pattern relative to [prefix], or null for every path beneath it.
  final String? filter;

  const AnnounceOptions({this.prefix = '', this.filter});

  MoqAnnounceConfig get _ffi =>
      MoqAnnounceConfig(prefix: prefix, filter: filter);
}

/// A connected MoQ session with publishing and subscription conveniences.
final class Moq {
  final MoqClient _client;

  /// The established raw MoQ session.
  final MoqSession session;

  Moq._(this.session, this._client);

  /// Connect to a relay at [url].
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
    MoqOriginProducer? publish,
    MoqOriginProducer? subscribe,
  }) async {
    final client = MoqClient();
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
  MoqBroadcastProducer createBroadcast(String path) =>
      session.publish().createBroadcast(path: path);

  /// Stream routes matching [options]; update prefixes stay relative to the origin.
  Stream<MoqAnnounceUpdate> announcements({
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
  MoqAnnounceConsumer announced({
    AnnounceOptions options = const AnnounceOptions(),
  }) => session.consume().announced(config: options._ffi);

  /// Wait for a broadcast announced at exactly [path].
  MoqAnnouncedBroadcast announcedBroadcast(String path) =>
      session.consume().announcedBroadcast(path: path);

  /// Resolve an existing broadcast at [path].
  Future<MoqBroadcastConsumer> requestBroadcast(String path) =>
      session.consume().requestBroadcast(path: path);

  /// The connection epoch: 1 for the connect that built this session, one more
  /// on each reconnect. A server-accepted session stays at 1.
  int get epoch => session.epoch();

  /// The session's bandwidth allocator.
  ///
  /// Every call returns a handle to the same registry. [MoqBandwidth.reserve]
  /// a share for an app-owned encoder; dropping the [MoqReservation] hands
  /// the room back.
  MoqBandwidth bandwidth() => session.bandwidth();

  /// Gracefully close the session and stop the client.
  void close() {
    session.shutdown();
    _client.cancel();
  }
}
