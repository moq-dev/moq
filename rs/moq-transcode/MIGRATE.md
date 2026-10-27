# Migrating moq-transcode

How to update your code for each breaking change, newest first.

## Unreleased

### `moq_net::Path` has no lifetime

`Config::source` is an `Option<moq_net::path::Relative>`: build it with `Relative::new(".")?` instead of `RelativeOwned::from(..)`. See moq-net's migration for the rest.

### Ladder resolution names its short side

`Rung::height` is `short_side`, and `Rung::new` takes the shorter coded dimension, not always the height.
The scalar fields in `ladder::Error::Empty` and `Unordered` are `short_side` and `below_short_side`.
For landscape output the value is unchanged; for portrait output it is the width, and the other dimension follows the source aspect ratio.

### OpenH264 is opt-in (#5771)

The default features drop `openh264`, so a GPU-less host has no H.264 encoder or decoder.
Add `features = ["openh264"]` to keep the software fallback.
