`doc/` is the entry point to the repo: what exists, what it is for, and how to start.
It is not the reference. Rustdoc, TSDoc, `--help`, and `drafts/` are, and they sit next to the code so they stay accurate.

- Document features, and the behavior a user must know to choose or use one.
- Leave method and field lists, wire encodings, error codes, and per-version accounting to the API docs and drafts. Link to them.
- A code change touches `doc/` only when it changes what a reader would do. A fix, a new knob, or an edge case belongs in the API doc.
- Edit the paragraph that already covers the topic instead of appending a new one. When a page grows, cut something.
- Weight follows importance to a user, not how recently something changed.
- State a claim's conditions with it: opt-in or experimental versions, and the paths it does not cover.
- Describe only what ships. Plans live in quests.
