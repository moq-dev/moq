# [L] AF_XDP packet I/O in moq-sock

## Goal

`moq-sock` can attach an XDP program to an interface that redirects TCP
packets for one port, IPv4 and IPv6, to an AF_XDP socket for each RX queue,
and passes every other packet to the kernel. `moq-tokio` drives each socket's
rings from a task. Detaching, whether on drop or when the process dies, leaves the
interface as it was.

## Plan

Settled in [the epic](/quest/m5/userspace-tcp/README.md): no C dependencies, and
tokio first. Recommendations, by the agent:

- The program is BPF bytecode encoded in Rust and loaded with `bpf()`, and it is
  verified to load on the kernels we support. It parses Ethernet, IPv4 or
  IPv6, and the TCP destination port, then calls `bpf_redirect_map` into an
  `XSKMAP` keyed by RX queue. Attach it through a BPF link, so the kernel
  detaches it when the process exits.
- Prefer zero-copy native mode, and say so at attach time: the setup reports
  native, copy, or generic mode, and the relay logs which one it got. A
  mode the operator didn't ask for is an error, per the epic's fail-loud
  decision.
- L2: reply to the source MAC of the frame that opened the flow, which is the
  gateway's, rather than reading the kernel's neighbor table over netlink.
  Under asymmetric routing that is the wrong hop; say so in the docs rather
  than add netlink up front.
- Use RSS so each 4-tuple stays on one queue and so on one task, with no shared
  connection table.
- Test on a veth pair in a netns, where XDP works in native mode on any
  kernel. It needs `CAP_NET_ADMIN` and `CAP_BPF`, so it runs on the
  self-hosted Linux runner and skips loudly elsewhere.

## Hosts

- linux - AF_XDP is Linux only
