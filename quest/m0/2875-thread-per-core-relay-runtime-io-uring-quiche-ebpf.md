# [XL] Thread-per-core relay runtime: io-uring + quiche, eBPF connection steering, rename moq-native to…

## Goal

Implement and verify the behavior tracked in [#2875](https://github.com/moq-dev/moq/issues/2875)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

### Motivation

The first big customer plans to use MoQ as a message bus for chat. Egress per connection is tiny, so the cost driver is CPU per connection and CPU per message, not bandwidth. The current relay runs one work-stealing tokio runtime: every packet potentially crosses threads, every wakeup is a candidate context switch, and every UDP datagram is a syscall. The goal is a thread-per-core mode where each connection is pinned to one thread for its whole life, I/O is batched via io-uring, and incoming packets are steered to the owning thread in the kernel.

Media workloads (1:1 and 1:N) must not regress and should also benefit.

#### What the codebase already gives us (verified)

- **`moq-net` is already runtime-agnostic on `dev`.** #2736 (`refactor(net)!: require the poll-based transport interface`) drives transports purely through `web_transport_trait::poll::{Session, SendStream, RecvStream}` (v0.4). `moq-net` has no tokio dependency; every future is kio-based and a session `Driver` can be stepped with `kio::Waiter` from any executor. This part of the plan is confirmed, not speculative.
- **The one remaining runtime tie is time.** `kio::time::Deadline` (and `moq-net`'s bandwidth sampling, control-stream timeout, and subscription linger) bottom out in `web_async::time`, which is tokio's time driver on native and panics outside a tokio runtime. A non-tokio runtime needs a pluggable timer backend in kio.
- **Send bounds:** on native, `moq-net` boxes internal futures as `Send` (`MaybeSendBox` = `BoxFuture`); only wasm gets the `LocalBoxFuture` path. Thread-per-core does not require `!Send` (pinned `Send` types work; locks just become uncontended), but `!Send` internals (Rc, thread-local wakers) are off the table until that cfg split is widened.
- **A quiche backend already exists** (`web-transport-quiche`, the `quiche` feature of `moq-native`), including raw QUIC. quiche is sans-IO by design: the application owns sockets, timers, and packet pacing, which is exactly the contract an io-uring driver wants.
- **`moq-bench` exists** and its `F=0` mode (lone JSON keyframe groups) is nearly the chat workload already. It has no CPU-side measurement yet.
- **No `SO_REUSEPORT` anywhere today**, only `SO_REUSEADDR`.

#### Decisions taken (from design review)

1. **Success bar:** baseline first. Milestone 0 measures where CPU actually goes today, then we commit to a concrete multiplier (e.g. Nx connections per core). No optimization is judged without a number from the rig.
2. **QUIC stack: quiche.** Battle-tested at exactly this architecture (thread-per-core, SO\_REUSEPORT, eBPF steering) at Cloudflare. noq-proto and quinn-proto remain possible later since everything goes through `web-transport-trait`, but the io-uring driver is built around quiche.
3. **Milestone 1 is thread-per-core tokio**, not io-uring. N pinned `current_thread` runtimes with per-thread `SO_REUSEPORT` sockets and eBPF steering validates the architecture, keeps timers and WebSocket working, and isolates how much win comes from pinning alone vs io-uring itself.
4. **Scope: QUIC first, parity later.** The new runtime owns QUIC on :443/UDP only. WebSocket/WSS, TCP, UDS, iroh, and cert reload stay on a tokio runtime in the same process. The plan commits to eventually porting the fallbacks so tokio can be dropped from the relay, but not in v1.
5. **Deployment floor: our own infra.** Modern kernels (6.x), CAP\_BPF available. The io-uring path hard-requires its features and refuses to start otherwise. Self-hosters keep the tokio relay; no fallback detection logic.
6. **Send story: decided by data.** Ship Milestone 1 with `Send` types, profile atomic/lock overhead under the chat workload, and only widen the wasm `!Send` path to native (feature-gated) if it shows up as real CPU.
7. **io-uring shape: direct implementation.** Monoio is rejected for this work because its public UDP API does not expose the ancillary-data and batching primitives we require. Build the spike directly on the `io-uring` crate.
8. **WebTransport layer: `web-transport-proto` over quiche.** `web-transport-quiche` is coupled to `tokio-quiche` and spawns tokio tasks, so it is not the abstraction for the new runtime. Reuse the protocol state machine from `web-transport-proto` and implement its I/O against sans-I/O quiche.
9. **UDP batching is required, not optional polish.** The tokio/control implementation must support `recvmmsg`; the io-uring implementation uses multishot `recvmsg` with provided buffers as its equivalent. Both must preserve ancillary data and UDP GRO. Egress must use `sendmsg` with `UDP_SEGMENT` GSO.
10. **Time is an M3 prerequisite.** Replace the concrete tokio sleep inside `kio::time` with an executor-provided deadline backend. Tokio and wasm keep their existing behavior; the direct runtime drives deadlines with timeout SQEs or a measured local timer structure.

#### Architecture sketch

- N worker threads, each pinned to a core. Each binds its own UDP socket to :443 with `SO_REUSEPORT`, giving N sockets in one reuseport group.
- An `sk_reuseport` eBPF program (`SK_REUSEPORT` + `BPF_MAP_TYPE_REUSEPORT_SOCKARRAY`) parses the QUIC header and steers by connection ID: server-chosen CIDs encode the owning socket index (a few plaintext bytes plus entropy, standard quiche-style routing).
- Initial packets carry a client-chosen DCID we cannot decode, so they hash to an arbitrary socket. Whichever thread receives the Initial owns the connection forever and issues CIDs that encode its own index. No cross-thread handoff, no migration.
- Retransmitted Initials must reach the same thread before our CID takes effect: steer them by hashing the client-chosen DCID consistently in the eBPF program, so retries land on the thread that saw the first packet.
- Each thread runs: io-uring UDP rx (multishot `recvmsg` with a provided-buffer ring, the io-uring equivalent of `recvmmsg`, plus GRO) -> quiche packet processing -> `moq-net` `Driver`s polled on a local task queue -> quiche egress -> io-uring UDP tx (`sendmsg` + `UDP_SEGMENT` GSO, zerocopy only where it wins). Timers come from timeout SQEs or a measured local timer structure feeding quiche's `on_timeout` and kio deadlines.
- Fanout (1:N) across threads goes through the existing shared `Origin` model: cross-thread wakeups are memory traffic, not syscalls. Measured, not assumed, in the benchmarks.
- The same process keeps a small tokio runtime for WebSocket/TCP fallback, stats, signal handling, and cert hot-reload.

#### Non-goals

- **Routing Initials to threads with spare capacity.** Accept hash placement; revisit only if the benchmarks show real imbalance.
- **Customer/tenant affinity (SNI, token root, path).** Invites hot keys; a busy tenant must be able to span cores.
- **Full moq-native parity in the new crate for v1** (see decision 4).
- **Fallback detection for hardened/exotic environments** (see decision 5).

### Milestones

Each milestone is a tracking issue with its own PRs; this issue is the epic. Every milestone ends with the same benchmark suite run on the same rig, so the deltas compose into one story.

#### M0: Measure. Chat + media benchmark suite and the tokio baseline

The whole project is gated on this: if profiling shows CPU goes to crypto or memcpy rather than syscalls and scheduling, the plan changes before we build anything.

- Extend `moq-bench` with a chat profile: thousands of connections, tiny frames, low per-connection rate, `F=0` style groups, plus fanout matrices for 1:1 and 1:N (N spanning small rooms to large ones, e.g. 10 / 1k / 100k subscribers).
- Keep/refresh the media profiles (video-sized frames, keyframe cadence) for 1:1 and 1:N.
- Add CPU measurement to the harness: CPU per connection, CPU per message, context switches (`perf stat`), syscall share, flame graphs of the relay under each profile. Results in a machine-readable format so runs are diffable.
- A `just` recipe to run the matrix against a relay; document the reference hardware.
- **Exit criteria:** baseline numbers for the current multi-thread tokio relay are written down, the dominant CPU sinks are identified, and the target multiplier for the project is committed.

#### M1: Thread-per-core tokio relay

- Relay mode: N pinned threads, each a `current_thread` tokio runtime, each with its own `SO_REUSEPORT` UDP socket on :443.
- The `sk_reuseport` eBPF steering program and its loader: CID-encoded socket index for established connections, consistent DCID hash for Initials. This program is runtime-agnostic and carries over unchanged to the io-uring milestones.
- CID generation in the quiche backend encodes the thread index (quinn can follow later if we care).
- Wire it behind a relay config flag; the default stays the multi-thread runtime.
- Profile: how much did pinning + no work-stealing buy on the M0 suite? What share of remaining CPU is atomics/locks (decides the `!Send` question), syscalls (decides how much io-uring can still win), crypto?
- **Exit criteria:** benchmark delta vs M0 recorded; Send/!Send decision made from the profile; go/no-go on the io-uring investment made from the syscall share.

#### M2: Crate split. moq-native becomes moq-tokio

- Rename `moq-native` to `moq-tokio` (semver break, lands on `dev` per branch targeting rules).
- Extract the runtime-neutral pieces the new runtime will also need (TLS/cert plumbing, config types, address resolution, the CID codec and eBPF loader from M1) into a shared crate so `moq-tokio` and the future io-uring crate depend on common code instead of copying it.
- Keep `moq-native` as a deprecated re-export shim only if it is cheap; otherwise a clean break on `dev` with migration notes in the PR.
- Docs sweep per the cross-package sync table (`doc/lib/rs`, examples, justfiles).
- **Exit criteria:** tree builds with `moq-tokio`; no behavior change; downstream crates in the workspace migrated.

#### M3: Prove the direct io-uring data path and runtime prerequisites

Time-boxed. This milestone answers whether the required Linux batching path works and improves the actual quiche workload before the relay integration grows around it. There is no Monoio candidate.

##### M3a: Replace the native `kio::time` backend

- Consolidate every `moq-net` timer and clock read behind `kio::time`. The initial audit found direct `web_async::time` use in bandwidth sampling, GOAWAY enforcement, and the lite PROBE interval in addition to existing `Deadline` users.
- Replace `Deadline`'s concrete `web_async::time::Sleep` with an executor-selected timer registration. Selection must happen at runtime because tokio fallback work and io-uring QUIC coexist in one process.
- Keep the public surface small: callers continue to own and poll a deadline. Do not pass a timer provider through every `moq-net` type.
- Preserve tokio paused-clock tests and the wasm backend. Add a no-tokio test executor that proves a `moq-net` driver can arm, re-arm, cancel, and fire deadlines without entering a tokio runtime.
- Compare timeout SQEs with a local heap/wheel for cancellation churn and pacing precision. The ring is not automatically the winner for thousands of short-lived timers.
- **Exit criteria:** `kio::time::Deadline` and a representative `moq-net` driver run under the local executor with no tokio runtime; timer precision and cancellation behavior are measured and documented.

##### M3b: UDP batching ablation benchmark

- Build the direct per-thread loop on the `io-uring` crate. Receive with multishot `recvmsg` plus a provided-buffer ring, parse source/destination address, ECN, timestamps, and `UDP_GRO`, and re-arm cleanly after `IORING_CQE_F_MORE` clears or `ENOBUFS`.
- Add a `recvmmsg` receive-batching control to the tokio path. Multishot `recvmsg` is the io-uring equivalent, but the control is needed to separate receive batching from executor effects.
- Send quiche packet batches with `sendmsg` and a `UDP_SEGMENT` control message. Only coalesce packets with the same destination, source, ECN, pacing time, and segment size.
- Run an ablation matrix at a fixed offered load: single receive/single send, receive batching only, GSO only, receive batching + GSO, then the direct io-uring path. Toggle GRO separately. Record actual batch sizes, packets/syscall, ring submissions/completions, drops, `ENOBUFS`, CPU/message, CPU/Gbps, and latency.
- Test chat-sized datagrams and media-sized traffic. Treat zerocopy as a separate experiment because notification CQEs and buffer lifetime may cost more than the copy for small packets.
- Use a dedicated transport benchmark first, then repeat the winning configuration with the M0 workload. The M0 harness speaks MoQ and cannot directly drive a raw echo server without a transport mode/client.
- **Exit criteria:** all required socket features work on the deployment kernel and NIC; the ablation attributes the gain to receive batching and/or GSO; the direct path improves quiche CPU at the same offered load. If it does not improve end-to-end CPU, stop before M4.

##### M3c: quiche plus WebTransport proof

- Drive sans-I/O quiche directly from the M3b loop.
- Put `web-transport-proto` on top for the HTTP/3 CONNECT handshake and WebTransport session state. Do not depend on `web-transport-quiche` or `tokio-quiche` in the new path.
- Accept a session from an existing client and run bidirectional stream and datagram echo through it. Feed quiche timeout and pacing deadlines through the M3a backend.
- Keep the adapter shaped toward `web_transport_trait::poll`, but do not build the complete public transport crate in the spike.
- **Exit criteria:** the same benchmark client completes the WebTransport handshake and echo workload on tokio and direct io-uring; CPU, latency, loss, and syscall/ring-operation deltas are recorded on the same hardware.

#### M4: Integrate the proven io-uring runtime

- New crate (name TBD in the M4 PR, e.g. `moq-uring`): grow the M3 winner into the production runtime. It owns thread spawning/pinning, sockets, the ring, provided-buffer pools, the local executor, and the timer backend selected in M3a.
- Extract the runtime-neutral TLS/cert configuration, CID codec, eBPF loader, and socket configuration that remain private in `moq-tokio`. The M2 rename landed, but these shared pieces were not extracted.
- Implement the WebTransport-over-quiche adapter from M3c as `web_transport_trait::poll` session and stream types, then drive real `moq-net` `Driver`s.
- Relay integration behind the same config flag as M1: io-uring threads own QUIC, while the tokio side keeps WebSocket/TCP/UDS, stats, signals, and cert reload. Cluster/upstream QUIC dials happen from the io-uring threads too.
- **Exit criteria:** relay passes the existing integration and smoke tests in io-uring mode; the full M0 suite records deltas against M1; the required receive batching and send GSO remain enabled and observable.

#### M5: Hardening and continuous benchmarks

- Nightly benchmark runs on a dedicated box (rig details TBD; cloud CI runners are too noisy for CPU numbers), trended over time, regressions visible per commit range.
- Soak: multi-hour chat + media mixed load, memory stability, CID rotation, connection churn.
- Operational story: qlog in io-uring mode, `moq-stats` integration, graceful drain/shutdown of pinned threads.
- Rollout on our fleet behind the flag, then default for our deployment. `moq-tokio` remains the default for everyone else.
- If M1's profile said the `!Send` refactor pays: land it here (feature-gated `LocalBoxFuture` path in `moq-net`, Rc internals, thread-local kio wakers) and re-measure.
- **Exit criteria:** the M0 target multiplier is met or the gap is explained with a profile; benchmarks run nightly; the chat customer's projected load fits the promised hardware.

### Risks and open questions

- **WebTransport adapter scope:** `web-transport-proto` supplies protocol state, but the new path still owns the quiche stream/datagram adapter and its `web_transport_trait::poll` lifecycle. Prove the minimal boundary in M3c before designing the production public API.
- **Cross-thread fanout:** 1:N where subscribers live on many threads keeps the shared-memory origin model on the hot path. If M0/M1 show contention there, group fanout may need per-thread egress queues. Measured before designed.
- **io\_uring security posture** is fine for our fleet (decision 5) but permanently excludes some self-host environments; the tokio relay remains the answer there.
- **eBPF program lifecycle:** pinning, upgrade across relay restarts (reuseport group membership changes when threads restart), and observability when steering goes wrong.
- **Timer precision and cancellation:** quiche pacing wants fine-grained timers, while a timeout SQE per logical deadline may create excessive submission and cancellation traffic. M3a compares the ring with a local timer structure under realistic churn.
- **Provided-buffer ownership:** multishot receive depends on correct buffer-ring replenishment. Exhaustion, stale CQEs after cancellation, and GRO segment parsing need counters and stress tests.
- **Benchmark interpretation:** loopback UDP microbenchmarks prove feature support and give directional data, but only the fixed-load quiche and M0 runs decide whether the runtime is better.
- **Benchmark rig:** hardware not yet chosen (dedicated bare metal vs a fixed cloud metal instance). Owner input welcome on what we can dedicate.
- Fanout matrix ceilings (is 100k subscribers per broadcast a real chat shape, or is 10k the honest max?).

No wire format changes anywhere in this plan, so no `drafts/` updates are expected. Relay config/docs updates ride each milestone PR per the cross-package sync table.

## Required

- [#1073: Make Origin lifecycle caller-driven](/quest/m0/1073-make-origin-lifecycle-caller-driven.md) - complete the prerequisite issue first

## Closes

- [#2875](https://github.com/moq-dev/moq/issues/2875) - close this issue when the quest finishes
