# Migrating the Go moq-ffi module

How to update your code for each breaking change, newest first.

## Unreleased

### Regenerate the matching bindings

Documentation on `MoqAudioProducer::write` and `finish` changes their UniFFI metadata checksums.
Regenerate the Go sources and bundle the matching static libraries, then rebuild callers.
The method signatures and behavior are unchanged.
