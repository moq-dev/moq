# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository Overview

This is an IETF Internet-Draft repository for Media over QUIC (MOQ) protocol specifications authored by Luke Curley. It contains three draft documents:

- **draft-lcurley-moq-lite.md**: A simplified MOQ transport protocol for real-time conferencing
- **draft-lcurley-moq-hang.md**: Specification for handling connection hangs in MOQ
- **draft-lcurley-moq-use-cases.md**: MOQ protocol use cases documentation

## Build Commands

Run `make` to build/validate drafts. **`make` self-bootstraps its toolchain** —
on first run it clones the i-d-template submodule into `lib/`, creates a Python
venv (`lib/.venv`, for xml2rfc), and bundler-installs kramdown-rfc into
`lib/.gems`. No manual install of kramdown-rfc/xml2rfc is needed; you only need
system `python3` (with venv+pip), `ruby` (with `bundle`), a C compiler, and
network access on the first build. Do not claim the drafts can't be built — run
`make`. (A successful kramdown-rfc step also validates a draft's syntax.)

There is no separate format step: prettier is disabled (`.prettierignore` is
`**`), so editing the markdown is the whole workflow.

```bash
# Build all drafts (generates HTML and text versions)
make

# Build/validate a single draft (fast feedback while editing)
make draft-lcurley-moq-lite.txt

# Clean build artifacts
make clean

# Update GitHub Pages (typically done via CI)
make gh-pages
```

## Development Workflow

1. **Prerequisites**: The build system requires i-d-template tools. See [setup instructions](https://github.com/martinthomson/i-d-template/blob/main/doc/SETUP.md).

2. **Building drafts**: Running `make` will:
   - Initialize the i-d-template submodule if needed
   - Convert Markdown drafts to RFC XML format
   - Generate HTML and text versions
   - Output files: `draft-*.html` and `draft-*.txt`

3. **Git workflow**:
   - Main branch: `main`
   - Pull requests automatically trigger CI builds
   - GitHub Pages updates on push to main
   - Draft versions are released to the IETF datatracker by pushing a tag: `draft-lcurley-<name>-XX` (e.g. `draft-lcurley-moq-lite-04`)

## Architecture

This is a standards documentation project, not a software implementation. Key components:

- **Draft documents**: Root-level `draft-*.md` files in kramdown-rfc format
- **Build system**: Uses i-d-template via git submodule in `lib/`
- **CI/CD**: GitHub Actions workflows handle building and publishing
- **Output**: Published to GitHub Pages at https://kixelated.github.io/moq-drafts/

## Working Group Context

- Part of IETF Media Over QUIC (MOQ) Working Group
- Discussion: moq@ietf.org mailing list
- Related to main MoqTransport specification (moq-lite is a simplified alternative)

## Important Notes

- Follow IETF contribution guidelines (BCP 78/79)
- Draft format uses kramdown-rfc with YAML frontmatter
- References are managed via bibxml includes
- Do not edit generated files (*.html, *.txt)
- When making a wire-format or semantic change to a draft, add a bullet to its changelog appendix (e.g. `# Appendix A: Changelog`) under the in-progress version's section. Drafts without a changelog section (typically unreleased ones) don't need one.
- Keep changelog bullets concise and factual; list what changed without explaining the motivation or design reasoning.
