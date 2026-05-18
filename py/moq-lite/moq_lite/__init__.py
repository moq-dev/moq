"""Deprecated: moq-lite has been renamed to moq-net.

This package re-exports ``moq_net`` for backwards compatibility and will not receive
further updates. Please migrate your dependencies from ``moq-lite`` to ``moq-net``.
"""

import warnings

warnings.warn(
    "The 'moq-lite' Python package has been renamed to 'moq-net'. "
    "This shim re-exports moq_net and will not receive further updates. "
    "Please migrate to moq-net.",
    DeprecationWarning,
    stacklevel=2,
)

from moq_net import *  # noqa: E402, F401, F403
