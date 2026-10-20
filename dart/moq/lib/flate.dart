/// Opaque snapshot and stream tracks, mirroring the moq-flate crate.
library;

import 'package:moq_ffi/moq_ffi.dart';

/// Publishes the latest opaque payload on a track it takes over.
typedef SnapshotProducer = MoqFlateSnapshotProducer;

/// Publishes an ordered opaque payload log on a track it takes over.
typedef StreamProducer = MoqFlateStreamProducer;

/// Configures compression and the opaque payload media type.
typedef Config = MoqFlateConfig;
