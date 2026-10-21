# Migrating the Dart moq\_ffi package

How to update your code for each breaking change, newest first.

## Unreleased

### Regenerate the matching bindings

Documentation on `MoqAudioProducer::write` and `finish` changes their UniFFI metadata checksums.
Keep the generated Dart code and native library together.
For a custom native build, regenerate the matching bindings with `just dart generate`.
The method signatures and behavior are unchanged.
