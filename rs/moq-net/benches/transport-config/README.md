# Driver panic boundary CPU measurement

The existing session benchmark sweeps 1/4/16 publishers and 1/4/16 viewers,
with one relay, 64-byte frames, subscribe/fetch, bounded/unbounded stream windows,
and lite06/lite07. Each measured group includes its real protocol driver polls.
The CSV retains all 72 cases in nanoseconds of thread CPU per delivered group.

```sh
SESSION_CPU=1 cargo bench --locked -p moq-net --bench session -- session_delivery_group_cost_cpu --quick --noplot
```

Measured sequentially in the Nix shell on an Intel Core Ultra 7 270K Plus
(24 cores, Linux), before the driver
unwind guard, after it, and once more without changing that guard. Criterion
`--quick` provides only a small sample, and other work shared the host.
Before/after median relative change was -4.09% (case range -44.78% to -0.09%).
The unchanged-guard A/A median was +3.21% (range -0.34% to +72.68%). These runs
show no reliable effect distinguishable from host/order noise and establish no
speedup. They do not measure full media decoding, network impairment, or throughput.
The new guard adds a thread-unwinding check at the actual protocol poll boundary;
there is no extra session task, handle, or per-peer state.
