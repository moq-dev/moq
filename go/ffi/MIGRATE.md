# Migrating the Go moq-ffi module

## Unreleased

### Separate datagram tracks

Generated bindings now expose separate `MoqDatagramProducer`, `MoqDatagramConsumer`, `MoqDatagramInfo`, `MoqDatagramRequest`, and `MoqDatagramRequests` types. Create or subscribe through the broadcast's datagram methods, write with `append` / `insert`, and read with `recv`; stream-track datagram methods are removed. Demand is aggregate subscriber priority, and the broadcast request handler has a separate `datagrams` queue. Regenerate and rebuild native bindings together with your wrapper.

### Follow the core API migration guide

These bindings are generated from moq-ffi. Apply every change in the
[moq-ffi migration guide](../../rs/moq-ffi/MIGRATE.md), using this language's generated casing.
Regenerate the bindings and rebuild the matching native library together; method metadata checksums are not a separate API migration.
The [hand-written wrapper](../wrapper/MIGRATE.md) has its own migration guide.
