/// JSON tracks, mirroring the `moq-json` crate.
///
/// Each type wraps a track: a producer takes over a `TrackProducer` (the
/// snapshot and stream producers also advertise it in the broadcast's catalog,
/// which has no window mode), and a consumer takes over a `TrackConsumer` that
/// has not read a group yet. Values cross as JSON strings. Compression is the
/// track's: publish JSON with `MoqTrackInfo(compress: true)`, and a consumer
/// reads it with no option of its own. Import it with a prefix, since
/// `dart:convert` has a `json` too:
///
/// ```dart
/// import 'package:moq/json.dart' as moq_json;
///
/// final status = moq_json.SnapshotProducer(
///   broadcast: broadcast,
///   track: broadcast.publishTrack(
///     name: 'status',
///     info: MoqTrackInfo(compress: true),
///   ),
///   config: moq_json.SnapshotConfig(),
/// );
/// ```
library;

import 'package:moq_ffi/moq_ffi.dart';

/// Publishes lossy latest-value JSON snapshots on a track it takes over.
typedef SnapshotProducer = MoqJsonSnapshotProducer;

/// Consumes each JSON snapshot state from a track it takes over, or skips to
/// the newest with `latest()`.
typedef SnapshotConsumer = MoqJsonSnapshotConsumer;

/// Configures a lossy latest-value JSON producer.
typedef SnapshotConfig = MoqJsonSnapshotConfig;

/// Publishes a lossless stream of JSON records on a track it takes over.
typedef StreamProducer = MoqJsonStreamProducer;

/// Consumes a lossless stream of JSON records from a track it takes over.
typedef StreamConsumer = MoqJsonStreamConsumer;

/// A JSON string read from a track, with the timestamp of the frame that
/// carried it in microseconds, or null when untimed. Rounded down to a whole
/// microsecond, so a tick on a track like 90 kHz may not survive a read and
/// republish exactly.
typedef Value = MoqJsonValue;

/// Publishes a sliding window of JSON records on a track it takes over, pushed
/// at the back and popped from the front.
typedef WindowProducer = MoqJsonWindowProducer;

/// Consumes each push, pop, and skip of a JSON window from a track it takes
/// over. Group rolls never surface.
typedef WindowConsumer = MoqJsonWindowConsumer;

/// Configures a JSON window producer.
typedef WindowConfig = MoqJsonWindowConfig;

/// One change to a window: [WindowEventPush], [WindowEventPop], or
/// [WindowEventSkip].
typedef WindowEvent = MoqJsonWindowEvent;

/// A record joined the window at its absolute index.
typedef WindowEventPush = PushMoqJsonWindowEvent;

/// The records in `[start, end)` left the window.
typedef WindowEventPop = PopMoqJsonWindowEvent;

/// The records in `[start, end)` will never be delivered to this reader.
typedef WindowEventSkip = SkipMoqJsonWindowEvent;

/// A [WindowEvent] and the timestamp of the frame that carried it, in
/// microseconds, or null when untimed.
typedef WindowTimed = MoqJsonWindowTimed;
