`doc/` introduces features to users. It is not the reference: rustdoc, TSDoc, `--help`, and `drafts/` are, so link to them instead of repeating method lists, wire encodings, or error codes.

- Touch `doc/` only when a change alters what a user would do. A fix, a new knob, or an edge case belongs in the API doc.
- Edit the paragraph that covers the topic instead of appending. When a page grows, cut something.
- Weight follows importance to a user, not recency.
- State a claim's conditions with it: opt-in or experimental versions, and the paths it does not cover.
- Describe only what ships.
