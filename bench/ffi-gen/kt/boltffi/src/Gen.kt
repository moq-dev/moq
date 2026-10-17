package bench

// The runner's names for BoltFFI's bindings.
typealias MoqBroadcastProducer = dev.moq.gen.MoqBroadcastProducer
typealias MoqFrame = dev.moq.gen.MoqFrame
typealias MoqOriginProducer = dev.moq.gen.MoqOriginProducer

fun noop() = dev.moq.gen.noop()

suspend fun noopAsync() = dev.moq.gen.noopAsync()

suspend fun yieldOnce() = dev.moq.gen.yieldOnce()
