# [S] Adopt usage-rs 7

## Goal

The workspace builds on usage-rs 7. Its derive emits `::usage_rs` paths and
no longer reads `Cargo.toml`, so the `usage = { package = "usage-rs" }` alias
goes and every user names the crate `usage_rs`.
