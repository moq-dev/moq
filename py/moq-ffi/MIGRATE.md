# Migrating the Python moq-ffi package

How to update your code for each breaking change, newest first.

## Unreleased

### Regenerate the matching bindings

Documentation on `MoqAudioProducer::write` and `finish` changes their UniFFI metadata checksums.
Rebuild or reinstall the complete wheel so its generated Python code and native library match.
The method signatures and behavior are unchanged.
