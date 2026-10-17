# [XS] Delete NewReno

## Goal

`moq_quic::congestion::{NewReno, NewRenoConfig}` and `new_reno.rs` are
gone. MoQ names congestion families, never algorithms, and no family maps
to NewReno, so the controller is never instantiated and is 0% covered.

## Plan

Found by the 2026-10-16 coverage survey; kept out of the coverage epic
because the net does not change behavior. A breaking public API removal,
so it belongs to the m1 cut and needs a MIGRATE.md entry. Update the
`congestion_controller_factory` docs in `config/transport.rs` that name it.
