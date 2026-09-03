# [L] Explicit datagram insertion

## Goal

The explicit-sequence datagram API is named and shaped as insertion in TypeScript and Rust, with append reserved for automatic allocation.

The existing append helpers remain the convenient default, while an asynchronous transform can choose the final transport identity before producing ciphertext.

## Plan

- Replace the current relay-oriented `writeDatagram(Datagram)` and `write_datagram(Datagram)` names with matching `insertDatagram(sequence, timestamp, payload)` and `insert_datagram(sequence, timestamp, payload)` model APIs. They preserve the supplied sequence and current model behavior; `appendDatagram` and `append_datagram` remain the next-sequence convenience. This is API cleanup, not a cryptographic prerequisite.
- Preserve the shared group/datagram sequence namespace, exact insertion, gaps, out-of-order forwarding, counter advancement, and best-effort duplicate behavior already available to relays. The E2EE wrapper owns one producer, allocates monotonically unique identities, and commits its ordered encryption queue without imposing crypto policy on the transport model.
- Keep encryption, key state, and retry policy out of `moq-net`. Test exact IDs, gaps, duplicates, stale insertion, interaction with groups and append helpers, cloned producers, cancellation, and the moq-lite wire. Preserve the existing explicit non-delivery on MoQ Transport, which has no datagram mapping.
- Land the Rust and TypeScript API together and run their cross-language transport matrix, rather than letting the two languages drift or adding a consumer-local shim.
