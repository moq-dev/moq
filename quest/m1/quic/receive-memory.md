# [XL] Bound incomplete messages with QUIC connection credit

## Goal

Native MoQ receivers bound incomplete frames and protocol messages by the
connection memory allowance, returning connection credit only when each
complete unit is parsed and handed off or discarded. When concurrent partial
messages prevent progress, the receiver cancels streams by MoQ priority and
continues. This works with Rust and JavaScript senders through tokio and
io_uring; completed media caching remains separately governed.

## Plan

Decided in the 2026-10-10 interview: cover headers and frame/group data alike.
The frame preallocation budget only bounds speculative allocation, and the
cache has soft limits; neither bounds aggregate incomplete messages. Use
peer-visible QUIC flow control instead of a separate header quota error.
The native transport primitive is a separate prerequisite; do not activate
whole-message retention without its recovery policy in this quest.

Hold connection credit through reader buffers, type/length prefixes, headers,
extensions, and frame assembly. Attribute read-ahead bytes to the next unit
rather than releasing them with the completed one. Release on complete-unit
handoff, not after application playback or cache eviction. Avoid retaining
an entire multi-frame group as one unit. Account for received-but-unread and
out-of-order transport bytes as well as parser-held bytes; they share the
same allowance. Speculative allocations and copies need separate accounting
or bounds: QUIC byte credit is not a claim about total process RSS.

Keep the existing stream-window setting and honor explicit overrides.
Give the native path a very large default so it normally adds no round trips;
stream credit is replenished on reads, independently of message completion.
Do not remove or ignore the setting without benchmark evidence and a new
maintainer decision. The connection memory allowance is the shared cap.

Require that allowance to fit every supported encoded message, including
framing and extensions. Validate configuration and align native defaults
rather than discovering an impossible frame after receiving it. The current
32 MiB frame limit exceeds io_uring's 16 MiB connection default; a legacy
64 MiB announcement body also needs room for framing beyond 64 MiB. Derive
the minimum from the supported units, not just the payload limit. Lowering
a protocol/frame size limit is a separate maintainer decision, not an implicit
way around this requirement. Runtime memory-limit updates obey the same
minimum; already-advertised credit cannot be revoked. A rate limit can pace
a smaller amount of outstanding credit while the memory allowance remains
large enough for the complete unit.

MoQ selects victims; the transport only reports pressure and sends
STOP_SENDING. Do not add sender-initiated shedding. This keeps recovery
available with browser senders and avoids treating intentional rate pacing
as receiver deadlock. The sender's required RESET_STREAM response is still
normal QUIC behavior, not a second recovery policy.

Use the receiver's assembly and credit accounting to recover when partial
units exhaust progress capacity. Do not require peer DATA_BLOCKED, an
arbitrary timeout, loss, or a deadline. Demonstrate that useful work can
complete after cancellation, including when incoming streams keep arriving;
merely cycling through victims is not recovery. Distinguish bytes still able
to arrive under advertised credit from memory held by incomplete units.

Within the existing broadcast fairness domains, cancel lower-priority media
and less useful groups first, respecting subscription priority and group
order. Prefer low-priority attributed media over unidentified headers so
headers have a chance to identify themselves; unidentified streams remain
eligible if they prevent progress. Control streams have the highest priority
but are eligible too. If canceling an essential control stream terminates the
session under its protocol, surface that outcome instead of leaving tasks
parked. There is no permanently protected class or separate control reserve.
Use deterministic tie-breaking without comparing unrelated broadcasts' raw
priority values. Do not invent another subscription-priority API here.

Regression coverage must include many partial headers and frames filling the
shared allowance, a finite stream window smaller than a frame, the largest
supported encoded unit, read-ahead across unit boundaries, cancellation and
late reset/data races, unidentified and control-only pressure, and continued
progress after selecting victims. Exercise a JavaScript sender against both
native receive paths and run all-language interop. Measure retained bytes,
throughput, and completion latency across concurrent streams and frame sizes,
including finite versus very large stream windows. Wire checks and benchmarks
into CI; no wall-clock sleeps in unit tests.

Public API: native receive-credit integration and validation of the existing
window configuration; no new header-specific quota. Wire: existing QUIC
flow-control and STOP_SENDING/RESET_STREAM behavior, no MoQ format change.
Browser/iroh receive-side guarantees and completed-cache limits are outside
this quest. Update existing configuration and transport docs inline; no
separate guide quest is needed.

## Required

- [Retain connection credit](/quest/m1/quic/receive-credit.md) - core and native adapters support separate byte ownership and stream-credit release

## Related

- [IETF request headers](/quest/m1/ietf-dispatch-headers.md) - concurrent request parsing must not amplify unbounded partial-header retention
- [Scope track priority](/quest/m1/track-priority-scope.md) - broadcast fairness domains and subscription priority semantics
- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - share priority semantics without requiring sender-side recovery
- [QUIC bitrate caps](/quest/m2/rate-quic.md) - pace credit grants inside the independently bounded memory allowance
- [Relay peers get wider limits](/quest/m1/quic/peer-limits.md) - client and peer windows must both fit supported units
