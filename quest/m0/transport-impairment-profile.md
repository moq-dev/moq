# [M] Impaired-path profile for the transport drills

## Goal

The transport drills run against a bidirectionally impaired QUIC path (delay,
loss, and rate limit) with the impairment measured rather than assumed, and the
run removes every namespace and process it created on exit or cancellation.
The host's normal network path is never touched.

## Plan

`test/drill` already owns the scenarios and the sensitivity proof; this quest
only adds the impaired path they run on. The drills drive real QUIC over
loopback inside one process, so a private network namespace with `netem` on its
own `lo` impairs them unmodified: no veth pair, no proxy, and no way to reach
the host's interfaces. `unshare -rn` grants `CAP_NET_ADMIN` inside that
namespace without root on stock kernels; report the missing capability rather
than escalating when it does not.

- Linux only. On another host the recipe reports the profile as unavailable and
  exits nonzero for a strict run, rather than silently running unimpaired.
- Apply `netem` (delay, jitter, loss, reorder) plus a `tbf` rate limit to `lo`
  inside the namespace, so both directions are impaired. Never write a qdisc on
  a host interface, and never require `sudo` for the default profile.
- Record the applied settings, the seed, the kernel and `tc` versions, and a
  measured baseline (observed one-way delay, loss, and achieved rate) with the
  run's artifacts. A seed does not make kernel scheduling deterministic, so the
  record is evidence of what ran, not a promise of reproducibility.
- Do not substitute an HTTP interceptor or a TCP-only proxy: those cannot
  impair QUIC.
- Prove teardown: cancel a run mid-drill and show the namespace, the qdiscs,
  and every child process are gone, then rerun successfully on the same host.
- Assert the impairment actually applied before grading a drill. A profile that
  silently failed to install turns an impaired run into an unimpaired pass.

Acceptance: the three drills pass under a moderate profile and the recorded
baseline matches the requested one within a stated tolerance. A profile that
cannot install fails the run. Leave CI lane scheduling to the PR behavioral
gate selector, and do not use retries to make intermittent failures green.

## Related

- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - stores timelines, seeds, and traces
- [Runtime QA hosts](/quest/m2/runtime-qa-hosts.md) - provides Linux execution for the profile
