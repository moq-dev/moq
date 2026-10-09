/// JSON tracks, mirroring the `moq-json` crate.
///
/// Each type wraps a track: a producer takes over a `TrackProducer` and
/// advertises it in the broadcast's catalog, and a consumer takes over a
/// `TrackConsumer` that has not read a group yet. Values cross as JSON
/// strings. Import it with a prefix, since `dart:convert` has a `json` too:
///
/// ```dart
/// import 'package:moq/json.dart' as moq_json;
///
/// final status = moq_json.SnapshotProducer(
///   broadcast: broadcast,
///   track: broadcast.publishTrack(name: 'status', info: null),
///   config: moq_json.SnapshotConfig(),
/// );
/// ```
library;

import 'package:moq_ffi/moq_ffi.dart';

/// Publishes lossy latest-value JSON snapshots on a track it takes over.
typedef SnapshotProducer = MoqJsonSnapshotProducer;

/// Consumes reconstructed latest-value JSON snapshots from a track it takes over.
typedef SnapshotConsumer = MoqJsonSnapshotConsumer;

/// Configures a lossy latest-value JSON track.
typedef SnapshotConfig = MoqJsonSnapshotConfig;

/// Publishes a lossless stream of JSON records on a track it takes over.
typedef StreamProducer = MoqJsonStreamProducer;

/// Consumes a lossless stream of JSON records from a track it takes over.
typedef StreamConsumer = MoqJsonStreamConsumer;

/// Configures a lossless JSON stream track.
typedef StreamConfig = MoqJsonStreamConfig;
