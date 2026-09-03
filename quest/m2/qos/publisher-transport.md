# [M] Publisher connection transport

## Goal

A native MoQ publisher reports its sender-side transport health on a
connection-scoped channel, so each redundant publisher connection has an
independent RTT, bandwidth, loss, and traffic sample regardless of route
selection.

## Plan

Add a connection-scoped diagnostic channel for the publisher's
`ConnectionStats` upload sample: RTT, estimated send bandwidth, congestion or
loss counters, traffic-rate deltas, sampling interval, and sample age. The
receiving relay binds each sample to the authenticated session and its own
process-local connection id; the publisher cannot name another connection.
Do not route this channel as a broadcast or catalog track.

Keep publisher-reported values visibly distinct from relay-observed subscriber
stats, reject malformed or implausible samples, and let consumers classify
missing or stale telemetry as unknown. Treat the self-report as diagnostics
only, never as billing, authorization, or route-selection input. Version the
channel so older publishers remain usable without fabricating transport health.

Test a rate-limited and lossy publisher whose sender-side health degrades while
relay outbound control traffic remains small. With two redundant publisher
sessions for one broadcast, prove each relay-local connection retains its own
sample regardless of selected route.
