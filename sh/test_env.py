"""Subprocess environments for dispatcher fixtures."""

import os


def dispatch_env(**overrides):
    """Keep the caller's gate and Actions settings out of a dispatcher fixture."""
    env = dict(os.environ)
    for key in ("CI_STRICT", "IN_MOQ_DEV_SHELL", "ALLOW_HOST_TOOLCHAIN", "CHECK_JOBS", "CHECK_MODULES",
                "CHECK_FAIL_FAST", "GITHUB_BASE_REF", "GITHUB_ACTIONS", "GITHUB_STEP_SUMMARY"):
        env.pop(key, None)
    env.update(overrides)
    return env
