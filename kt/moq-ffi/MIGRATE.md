# Migrating the Kotlin moq-ffi package

How to update your code for each breaking change, newest first.

## Unreleased

### Regenerate the matching bindings

Documentation on `MoqAudioProducer::write` and `finish` changes their UniFFI metadata checksums.
Regenerate the Kotlin sources and package matching native libraries, then rebuild callers.
The method signatures and behavior are unchanged.
