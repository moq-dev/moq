# Migrating moq-video

How to update your code for each breaking change, newest first.

## Unreleased

### `Color` is a struct of matrix, range, primaries and transfer

The four variants become constants plus a range, and the new `color` module holds `Matrix`, `Range`, `Primaries` and `Transfer`.
`Bt601Limited` is `Color::BT601`, `Bt709Limited` is `Color::BT709`, and the `Full` variants set `range: Range::Full`.
Match on the field you need instead of the variant.
BT.601 now names its primaries (SMPTE 170M), and BT.2020 with PQ or HLG is representable.

```rust
// Before
let color = Color::Bt709Full;
let hd = matches!(color, Color::Bt709Limited | Color::Bt709Full);
// After
use moq_video::color::{Matrix, Range};
let color = Color { range: Range::Full, ..Color::BT709 };
let hd = color.matrix == Matrix::Bt709;
```

### `Encoder::encode` refuses a frame in another space

Decoded and resized frames now report their source's space from `Surface::color`, and `Encoder::encode` fails with `Error::ColorMismatch` when a frame's space differs from the one the encoder signals, rather than mislabelling it.
Open the encoder with the first frame's space, and reopen it when the space changes.

```rust
// Before
let encoder = Encoder::new(&encode::Config::new(width, height, rate))?;
// After
let mut config = encode::Config::new(width, height, rate);
config.color = frame.surface.color();
let encoder = Encoder::new(&config)?;
```
