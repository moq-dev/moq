# The moq-quic endpoint the QUIC Interop Runner drives. run.sh stages the
# context: the cargo-built `quic-interop` example under bin/, and the loader and
# libraries it links under nix/, at the store paths the binary names.
FROM docker.io/martenseemann/quic-network-simulator-endpoint@sha256:3b2b9e6fa317da238c8140a7b6d2bf8d4d45a8464b873eecac3ff2a32520b71a
COPY nix/ /nix/
COPY bin/quic-interop /usr/local/bin/quic-interop
COPY run_endpoint.sh /run_endpoint.sh
ENTRYPOINT ["/run_endpoint.sh"]
