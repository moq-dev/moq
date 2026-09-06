MoQ (Media over QUIC) is a next-generation live media delivery protocol providing real-time latency at massive scale. 
This is a polyglot monorepo with Rust (server/native) and TypeScript (browser) implementations.

# Layers

1. **quic** - Does all the networking. 
2. **web-transport** - (optional) A small layer on top of QUIC/HTTP3 for browser support. Provided by the browser or the `web-transport` crates.
3. **moq-net** - The networking layer on top of WebTransport/QUIC, implemented by CDNs. At session setup it negotiates one of two wire protocols: the simplified `moq-lite` protocol or the full IETF `moq-transport` protocol. Content splits into:
   - broadcast: a collection of tracks produced by a publisher
   - track: a live stream of groups within a broadcast.
   - group: a live stream of frames within a track, each delivered independently over a QUIC stream.
   - frame: a sized payload of bytes.
4. **hang** - Media-specific encoding/decoding in `moq-mux` that runs on top of `moq-net`. Contains:
   - catalog: a JSON track containing a description of other tracks and their properties (for WebCodecs).
   - container: each frame consists of a timestamp and codec bitstream
   - watch/publish: dedicated packages for subscribing/publishing with optional UI overlays
5. **application** - Users building on top of `moq-net` or `moq-mux`

Key architectural rule: The CDN/relay does not know anything about media. Anything in the `moq-net` layer should be generic, using rules on the wire on how to deliver content.

Note that we support WebSocket, TLS, UDS, etc as fallback transports using qmux.
These reliable transports won't be able to shed load as effectively during congestion.

# Project Structure
Top-level layout only. 
If working with specific languages, see `AGENTS.md` in their respective directories.

- `/rs/` - Rust crates for anything native, published as `moq-*`. See `rs/CLAUDE.md`.
- `/js/` - TypeScript packages for the browser, published as `@moq/*`. See `js/CLAUDE.md`.
- `/py/` - Python wrappers over `moq-ffi`. See `py/CLAUDE.md`.
- `/swift/`, `/kt/`, `/go/`, `/dart/` - language wrappers over `rs/moq-ffi` 
- `/cpp/` - C/C++ consumers of `libmoq`. 
- `/demo/` - demos and test media
- `/test/` - test harnesses that span more than one language or need a server
- `/doc/` - documentation site, make sure this stays up-to-date.
- `/drafts/` - Our MoQ protocols/extensions. See `drafts/CLAUDE.md`. See `https://datatracker.ietf.org/wg/moq/documents/` for upstream IETF drafts.
- `/quest/` - replacement for GitHub issues. See `quest/AGENTS.md`. Create new quests instead of GitHub issues.

Try to make cross-language changes at the same time.
For example, when you modify moq-ffi, also update the corresponding wrappers and documentation.

See `CONTRIBUTING.md` before making a PR.

# Required
- Focus on code maintainability. Refactor aggressively.
- Always dig into the root cause. Fix issues at the source when possible, avoiding workarounds.
- Reproduce bugs when possible so you can verify the fix.
- Never add random retries or sleeps, unless the root cause is unavoidable.
- Fix any recommended follow-ups, or create new quests if they're out of scope.
- Focus on the public API. It's the most important thing to get right to avoid breaking changes.
- If `foo` needs a new argument, never do `foo_with_x`. Make a breaking change to `foo` instead.
- Scrutinize any new APIs. Keep stuff private when possible.
- Avoid callback parameters. Prefer async handles instead.
- Let the type system do the heavy lifting; make misuse unrepresentable rather than merely documented.
- CLAUDE.md or AGENTS.md files should be minimal and split into scoped files based on language/library.

# Guidelines
- When adding new dependencies, use the newest stable version available.
- Prefer a maintained third-party crate over hand-rolling non-core functionality.
- Do not bump package versions unless the user explicitly asks for a version bump or release.
- No em dashes (—)
- Keep things concise. Avoid verbose comments unless they explain something non-obvious.
- Document every exported symbol. 
- Write the way you'd say it out loud, not the way a doc generator would. One short line is almost always enough.
- Comments must reflect the current state of the code, not its history.
- Maintain conventions, patterns, and naming.
- If you run into a workflow issue that also likely impacts other agents, fix it.
- Suggest simple improvements to CLAUDE.md/AGENTS.md if it could have avoided excess cycles.
- Avoid excessive repetition and verbosity.
- Inline helper functions if they are simple and self-explanatory.
- Retry loops should be avoided (fail fast) but if needed, use capped backoff with jitter.
- Don't include links to files in CLAUDE.md.
- Prioritize adding tests, unless they are flaky or obvious.
- Avoid functions with 4+ args. Prefer structs or tuples to pass multiple values.
- Focus on keeping naming simple and consistent.
- Scrutinize if any functionality is really needed, or if we could simplify/remove it.

# Development
PRs target `main` by default. 
`dev` is reserved for semver-breaking changes, except for `0.0.x` packages.

Before you start working on a PR, `git fetch origin` and set your upstream.
If you later realize a breaking change is needed, rebase on dev.

Use the Nix dev shell for project commands so (pinned) local tooling matches CI tooling. 
It should be used automatically via direnv.
`nix develop --command just ...`

```bash
just check        # Lint and compile what the branch changed
just test         # Test what the branch changed, same scope
just fix          # Auto-fix lint/formatting, same scope
```

These commands diff the branch against the base and only runs affected directories.
You can force a different base by passing it positionally.
