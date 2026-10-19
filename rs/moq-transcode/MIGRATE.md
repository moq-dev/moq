# Migrating moq-transcode

How to update your code for each breaking change, newest first.

## Unreleased

### OpenH264 is opt-in (#5771)

The default features drop `openh264`, so a GPU-less host has no H.264 encoder or decoder.
Add `features = ["openh264"]` to keep the software fallback.
