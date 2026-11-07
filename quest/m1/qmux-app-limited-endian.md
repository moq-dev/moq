# [XS] QMux reads the app-limited flag on big-endian Linux

## Goal

The in-tree qmux's `TcpStats::estimated_send_rate` reads `tcpi_delivery_rate_app_limited` correctly on every Linux byte order, so `RateFilter` keeps app-limited samples from lowering the estimate on big-endian targets too.

## Plan

`rs/qmux/src/socket.rs` reads `info.app_limited & 1`. The flag is the first bitfield in that byte (`tcpi_delivery_rate_app_limited:1`), which is bit 0 on little-endian targets and bit 7 on big-endian ones. Derive the mask from `target_endian`, and pin it with a layout test next to `linux_tcp_info_layout`.

It's unreachable today: qmux depends on `moq-crypto/provider` (rind), which builds only little-endian x86_64, aarch64 and armv7. It goes live once [rind's targets epic](https://github.com/moq-dev/rind/blob/main/quest/m2/adoption/targets/README.md) adds s390x, powerpc64 or MIPS.

Decided with the maintainer on 2026-11-07 (follow-up planning from [moq-dev/web-transport#478](https://github.com/moq-dev/web-transport/pull/478)) (✅ chosen):

- Placement: m4 [XS], fix now / ✅ m1 [XS] / gate on a rind big-endian release in m5.
- web-transport's copy: ✅ Related link only / no link.

## Related

- [web-transport's RateFilter port](https://github.com/moq-dev/web-transport/blob/main/quest/a1/qmux-rate-filter.md) - specifies the same endian-aware mask for crates.io `qmux`
- [qmux byte counters](/quest/m3/wire-stats/qmux.md) - extends the same `TCP_INFO` read
