# [M] Typed cluster peer configuration

## Goal

The public cluster configuration carries typed peer entries instead of
`Vec<String>`, with existing URL and symmetric-cost behavior implemented.
Later asymmetric routing does not change that public configuration type.

## Plan

Introduce a documented, extensible peer configuration value for
`ClusterConfig.connect`, retaining bare URL deserialization and accepting the
object shape settled by Peer reconfigure: `url`, `cost`, `egress`, and `token`.
Use a non-exhaustive Rust struct with construction through the URL and explicit
policy setters; do not expose the private `DialTarget` as configuration or
publish placeholder enum variants.

Normalize and implement the existing behavior in both forms. `egress` defaults
to `cost`; a different effective value is explicitly unsupported until m2.
An object token follows the existing inline credential path with identical
redaction and authorization behavior. Reject unknown object fields and objects
mixing policy with URL `cost` or `jwt` parameters. Preserve CLI URL input.

Use the same parser for static configuration and connect-api responses. Keep
canonical peer identity separate from dial policy, deduplicate equivalent
entries, reject conflicting entries, and retain last-good topology on failure.
Every supported policy change follows the existing session replacement path.
Never accept an `egress` value and ignore it.

Test URL compatibility, equivalent object forms, credentials, symmetric cost,
unsupported asymmetric cost, conflicting entries, and malformed updates using
the existing cluster CI tests. Update config documentation and public callers.
This changes the Rust configuration API; the accepted legacy configuration
format and wire behavior remain intact. Target dev.

## Related

- [Peer reconfigure](/quest/m2/pop-skipping/peer-reconfigure.md) - implements distinct charged and declared costs
