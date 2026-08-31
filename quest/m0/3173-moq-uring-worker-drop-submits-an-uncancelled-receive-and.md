# [S] moq-uring: Worker drop submits an uncancelled receive and stalls for 3.2 seconds

## Goal

Implement and verify the behavior tracked in [#3173](https://github.com/moq-dev/moq/issues/3173)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

On `dev` at `7494084a`, dropping a worker with a newly created UDP socket can take about 3.2 seconds and leak the receive operation.

`Handle::udp` arms the socket by writing its initial receive SQE into the submission queue. `Worker::drop` then calls `register_sync_cancel` before `pump()` submits staged entries:

https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/moq-uring/src/worker.rs#L309-L340

The io-uring API only considers requests already submitted to the ring for synchronous cancellation. The staged receive is therefore not matched. The first `pump()` submits it after cancellation has finished, leaving a live receive with no future cancellation request. Teardown waits through 64 iterations of 50 ms and then deliberately leaks the operation.

The initial receive is staged here:

https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/moq-uring/src/udp.rs#L486-L509

#### Reproduction

On Linux 7.1.3, a minimal `moq-uring` build without a QUIC backend ran:

```rust
let worker = moq_uring::Worker::new(Default::default()).unwrap();
let handle = worker.handle();
let io = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
let _socket = handle.udp(io, Default::default()).unwrap();

let start = std::time::Instant::now();
drop(worker);
println!("{:?}", start.elapsed());
```

Observed:

```
worker drop took 3.202129467s
```

The retained-socket path in `dropped_worker_rejects_operations` already creates this state, but the test has no bound on drop duration and does not detect the leak.

#### Expected

Worker teardown should submit or safely remove every staged operation before issuing the cancellation that is meant to cover it, then drain terminal completions without the fixed 3.2 second stall or leak. The ordering also needs to preserve the final-send guarantee being developed in #3141.

Add a regression that retains a socket across worker drop, asserts teardown completes promptly, and verifies the operation slab drains.

## Closes

- [#3173](https://github.com/moq-dev/moq/issues/3173) - close this issue when the quest finishes

## Related

- [#3141: uring: a close can be lost when the worker is torn down before its…](/quest/m0/3141-uring-a-close-can-be-lost-when-the-worker-is-torn-down.md) - related open work
