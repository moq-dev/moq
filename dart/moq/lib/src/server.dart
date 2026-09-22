import 'package:moq_ffi/moq_ffi.dart';

import 'aliases.dart';

/// A listening MoQ server with publishing and subscription conveniences.
///
/// Build one with [Server.listen]. Broadcasts created via [createBroadcast] are
/// served to incoming sessions, and [requests] streams each incoming [Request]
/// for the caller to accept or reject. [close] stops accepting new sessions;
/// in-flight sessions stay alive until their handles are dropped or cancelled.
final class Server {
  /// The underlying server handle.
  final MoqServer server;

  /// The bound local address, e.g. `127.0.0.1:4443`. Resolved by [listen].
  final String localAddr;

  final OriginProducer? _publishOrigin;

  Server._(this.server, this.localAddr, this._publishOrigin);

  /// Bind a server at [bind] and start accepting.
  ///
  /// With neither [publish] nor [subscribe] given, both sides share one origin,
  /// so a broadcast created here is also visible to sessions publishing into
  /// this server. Wiring either side opts out and isolates the two directions.
  static Future<Server> listen({
    String bind = '[::]:443',
    List<String>? tlsCert,
    List<String>? tlsKey,
    List<String>? tlsGenerate,
    OriginProducer? publish,
    OriginProducer? subscribe,
  }) async {
    final shared = publish == null && subscribe == null
        ? OriginProducer(config: OriginConfig())
        : null;
    final publishOrigin = publish ?? shared;
    final subscribeOrigin = subscribe ?? shared;

    final server = MoqServer();
    try {
      server.setBind(addr: bind);
      if (tlsCert != null) server.setTlsCert(paths: tlsCert);
      if (tlsKey != null) server.setTlsKey(paths: tlsKey);
      if (tlsGenerate != null) server.setTlsGenerate(hostnames: tlsGenerate);
      if (publishOrigin != null) server.setPublish(origin: publishOrigin);
      if (subscribeOrigin != null) server.setConsume(origin: subscribeOrigin);

      final localAddr = await server.listen();
      return Server._(server, localAddr, publishOrigin);
    } catch (_) {
      // listen() failed: don't leak the server handle.
      server.cancel();
      rethrow;
    }
  }

  /// Create a broadcast at [path], served to incoming sessions.
  ///
  /// Advertise it with `announce` after populating tracks. Throws when [listen]
  /// was given a [subscribe] origin but no [publish] one, since there is then
  /// nothing to serve from.
  BroadcastProducer createBroadcast(String path) {
    final origin = _publishOrigin;
    if (origin == null) throw StateError('no publish origin configured');
    return origin.createBroadcast(path: path);
  }

  /// SHA-256 fingerprints of the configured TLS certificates, hex-encoded.
  ///
  /// Useful for pinning a generated self-signed certificate in a browser via
  /// WebTransport's `serverCertificateHashes`.
  List<String> certFingerprints() => server.certFingerprints();

  /// Stream of incoming sessions.
  ///
  /// Each [Request] must be answered with `accept()` to complete the handshake
  /// or `reject(code:)` to refuse it; the returned session must be held to keep
  /// the connection alive. The stream ends when the server stops accepting.
  Stream<Request> requests() async* {
    while (true) {
      final request = await server.accept();
      if (request == null) return;
      yield request;
    }
  }

  /// Stop accepting new sessions and release the native server handle.
  void close() {
    server.cancel();
  }
}
