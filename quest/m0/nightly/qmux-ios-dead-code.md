# [XS] qmux builds clean for iOS

## Goal

`rs/qmux` compiles for the iOS targets `just swift simulator` builds with no
warnings: `socket.rs`'s `AsRawFd` import, `TcpStats::fd`, and
`TcpStats::borrow` stop showing as dead code there.

## Plan

They sit in `mod unix` under `#[cfg(unix)]`, but only the Linux and macOS
`sys` modules read them, so on iOS (unix, neither OS) they are unused. Gate
`TcpStats` and what only it uses on `any(target_os = "linux", target_os =
"macos")`, matching the platforms that implement it, rather than adding an
allow. Its `cfg(unix)` callers follow the same gate and report no socket
stats elsewhere, as they already do off unix: `qmux::tcp::adopt`
(`rs/qmux/src/tcp.rs`), the `lib.rs` re-export, and moq-relay's
`socket_stats` (`rs/moq-relay/src/web.rs`).

Decided 2026-10-26 (maintainer): fix qmux only. Making the Platform
`cargo check` steps and `just swift simulator` deny warnings is out of scope.

Found in Nightly
[40192013303](https://github.com/moq-dev/moq/actions/runs/40192013303)'s
macOS platform job, which passes only because those steps do not deny
warnings.

Public API: `qmux::TcpStats` stops existing on iOS, where it never worked.
Wire: none.
