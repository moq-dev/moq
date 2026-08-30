/// Idiomatic Dart and Flutter client for Media over QUIC.
library;

import 'package:moq_ffi/moq_ffi.dart';

export 'package:moq_ffi/moq_ffi.dart';

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
    MoqOriginProducer? publish,
    MoqOriginProducer? subscribe,
  }) async {
    final client = MoqClient();
    try {
      if (!tlsVerify) client.setTlsDisableVerify(disable: true);
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
      if (publish != null) client.setPublish(origin: publish);
      if (subscribe != null) client.setConsume(origin: subscribe);

      final session = await client.connect(url: url);
      return Moq._(session, client);
    } catch (_) {
      client.cancel();
      rethrow;
    }
  }

  /// Create and announce a broadcast at [path].
  MoqBroadcastProducer createBroadcast(String path) =>
      session.publisher().createBroadcast(path: path);

  /// Stream announcements whose paths begin with [prefix].
  Stream<MoqAnnouncement> announcements({String prefix = ''}) async* {
    final announced = session.consumer().announced(prefix: prefix);
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

  /// Return the raw announcement cursor for [prefix].
  MoqAnnounced announced({String prefix = ''}) =>
      session.consumer().announced(prefix: prefix);

  /// Wait for a broadcast announced at exactly [path].
  MoqAnnouncedBroadcast announcedBroadcast(String path) =>
      session.consumer().announcedBroadcast(path: path);

  /// Resolve an existing broadcast at [path].
  Future<MoqBroadcastConsumer> requestBroadcast(String path) =>
      session.consumer().requestBroadcast(path: path);

  /// Gracefully close the session and stop the client.
  void close() {
    session.shutdown();
    _client.cancel();
  }
}
