# moq-lite (deprecated)

> **This package has been renamed to [`moq-net`](https://pypi.org/project/moq-net/).**

The package has been renamed to `moq-net` to make clear that it is the **networking
layer** for Media over QUIC. Under the hood it negotiates either the `moq-lite` wire
protocol or the full IETF `moq-transport` protocol at session setup.

## Status

`moq-lite` now re-exports `moq-net` so existing code keeps working without changes.
**It will not receive further updates** — new features and breaking changes ship on
`moq-net` only. Migrate at your convenience.

## Migration

```bash
pip uninstall moq-lite
pip install moq-net
```

```python
# Before
import moq_lite as moq

# After
import moq_net as moq
```
