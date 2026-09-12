# Origin lookup CPU

## Goal

Measure and reduce origin lookup and update CPU under many broadcasts,
alternative sources, and announcement consumers. Separate path traversal,
route selection, and output-sized replay costs before changing structures.

Each quest is independently shippable and retains paired measurements and
CI correctness coverage. Share registered benchmark fixtures when available;
a useful finding that no optimization is warranted also completes a quest.

## Quests

- [Route-selection CPU](/quest/m2/origin-cpu/origin-index.md) - measure and optimize per-broadcast source selection
- [Origin local tree](/quest/m2/origin-cpu/origin-tree.md) - measure and optimize path traversal under churn

## Related

- [Relay memory](/quest/m2/relay-memory.md) - memory rather than lookup CPU
- [Path patterns](/quest/m2/path-patterns/README.md) - matching and authorization contracts
