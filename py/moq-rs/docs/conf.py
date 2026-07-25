"""Sphinx configuration for the moq (moq-rs) API reference.

Read the Docs builds this (see .readthedocs.yaml) and hosts the result. The
native moq_ffi extension is mocked, so the pure-python wrapper documents
without a Rust build. `just py docs` renders the same config locally.
"""

from pathlib import Path

import tomllib

# Pull name/version straight from the wrapper's pyproject so the docs never
# drift from the released metadata.
_pyproject = Path(__file__).resolve().parents[1] / "pyproject.toml"
_meta = tomllib.loads(_pyproject.read_text())["project"]

project = "moq"
author = "moq-dev"
release = _meta["version"]
version = release

extensions = [
    "sphinx.ext.autodoc",
    "sphinx.ext.autosummary",
    "sphinx.ext.napoleon",
    "sphinx.ext.intersphinx",
    "myst_parser",
]

# The wrapper is pure python; its only runtime dependency is the native
# moq_ffi extension. Mock it so autodoc can import `moq` without building the
# Rust crate. The re-exported FFI data types are documented on the Rust side.
autodoc_mock_imports = ["moq_ffi"]

# The re-exported FFI record/enum types (Frame, Catalog, Audio, ...) resolve to
# mocks, which autodoc flags. That is expected here, so silence just that
# category rather than the whole build.
suppress_warnings = ["autodoc.mocked_object"]

autosummary_generate = True
autodoc_typehints = "description"
autodoc_member_order = "bysource"
napoleon_google_docstring = True
napoleon_numpy_docstring = False

intersphinx_mapping = {"python": ("https://docs.python.org/3", None)}

templates_path = ["_templates"]
exclude_patterns = ["_build"]

html_theme = "furo"
html_title = "moq"

# A .md landing page next to reStructuredText API stubs.
source_suffix = {".rst": "restructuredtext", ".md": "markdown"}
