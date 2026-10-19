package dev.moq.flate

/** Publishes the latest opaque payload on a track it takes over. */
typealias SnapshotProducer = uniffi.moq.MoqFlateSnapshotProducer
/** Publishes an ordered opaque payload log on a track it takes over. */
typealias StreamProducer = uniffi.moq.MoqFlateStreamProducer
/** Configures compression and the opaque payload media type. */
typealias Config = uniffi.moq.MoqFlateConfig
