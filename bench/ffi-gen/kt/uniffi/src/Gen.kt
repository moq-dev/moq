package bench

// The runner's names for UniFFI's bindings.
typealias MoqBroadcastProducer = uniffi.moq_ffi_gen.MoqBroadcastProducer
typealias MoqFrame = uniffi.moq_ffi_gen.MoqFrame
typealias MoqOriginProducer = uniffi.moq_ffi_gen.MoqOriginProducer

fun noop() = uniffi.moq_ffi_gen.noop()

suspend fun noopAsync() = uniffi.moq_ffi_gen.noopAsync()

suspend fun yieldOnce() = uniffi.moq_ffi_gen.yieldOnce()
