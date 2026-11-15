# QUIC interop

Run `nix develop --command just test quic-interop` on Linux with Docker or
Podman. It builds the `quic-interop` example of `moq-tokio`, an `hq-interop`
(HTTP/0.9) file server and client on the same `quic::driver` production uses,
packs it into the [QUIC Interop Runner](https://github.com/quic-interop/quic-interop-runner)'s
endpoint image, and runs every runner case and measurement with moq-quic as the
server against itself and each peer, then as the client against each peer.

Narrow a local run to one pair and one case:

```sh
just test quic-interop --peers quic-go --cases handshake --roles server
```

`--peers moq-quic` is the self pair alone.

[`cases.toml`](cases.toml) pins the runner commit, the simulator, and every
peer image by digest, so a peer's release cannot change a result. It also says
what each cell must read: every case passes in both roles, except the ones its
`unsupported` tables list with the reason (the endpoint exits 127 for each,
which the runner reports as unsupported), and the cells its `known` entries
list with why: a peer's own `unsupported`, a moq-quic failure with the quest
that owns it, or a peer's failure that interop.seemann.io shows against other
stacks too. Any other result fails the run. A known failure may also pass,
since loss and timing make those cases probabilistic; the run prints each
such pass, and the owning quest deletes the entry once its fix lands.

The runner marks a client that fails against every server as unsupported;
the harness turns that off, since it would hide a moq-quic regression that
breaks against every peer.

The runner names its containers and networks, so a host runs one matrix at a
time; a second run waits for the first. Rootless Podman needs three changes
to the runner's Compose file, which the harness applies when `docker` is
Podman: no interface names, no memlock limit, and no SELinux labels on the
bind mounts.

Nightly runs one job per peer (`.github/workflows/quic-interop.yml`), and a
branch that changes moq-quic can run the same workflow by hand:
`gh workflow run quic-interop.yml --ref <branch>`. A failed
cell keeps the runner's logs, pcaps, TLS key logs, and moq-quic's own log in
the job's artifact; cells that read as expected drop theirs.

To bump a pin, change it in `cases.toml` (or the endpoint base image in the
`Dockerfile`), run the full matrix, and update `known` to match.
