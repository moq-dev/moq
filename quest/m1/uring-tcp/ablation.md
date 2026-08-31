# [M] Ring TCP ablation

## Goal

A written-down number comparing ring TCP against tokio TCP under the relay's
actual stream workload, on the same hardware. It decides whether the rest of
this line is worth building and what shape the port should take.

## Plan

Follow `echo_quiche`: a bench over the real worker, toggling one thing at a
time, not a synthetic microbenchmark alone. Drive it with the `moq-bench`
shapes the UDP path was measured on, including the `F=0` chat shape where the
cost is per message rather than per byte, since that is where a syscall per
read hurts most.

Vary at least: tokio (epoll) against ring; multishot `recv` with a provided
buffer ring against a read per completion; batched writes against a write per
frame; and registered fixed files against fd lookups. Record CPU per message,
CPU per Gbps, syscalls per message, ring submissions and completions, and
latency, in the same machine-readable form the UDP matrix uses.

Exit criteria: the ablation attributes the gain (or the absence of one) to
specific mechanisms, and the result is recorded where the next two quests can
be judged against it. A result showing no meaningful win is a valid outcome
and abandons the rest of the line.
