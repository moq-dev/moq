# The final stage is scratch: the package's nix closure and a symlink to its
# binary. There is no shell or nix runtime.
#
# Building the image with Nix itself is a poor fit here: cross-compiling those
# images is painful (especially from macOS), and
# `docker build --build-arg package=moq-relay .` should work without a local
# Nix install.
FROM docker.io/nixos/nix:latest AS builder
ENV NIX_CONFIG="experimental-features = nix-command flakes"

WORKDIR /build

# Required: the flake package to publish, such as moq-relay or moq.
ARG package

RUN test -n "${package}" || { printf '%s\n' "error: the package build-arg is required" >&2; exit 1; }

COPY . .

# Copy the closure to a rootfs, then point a fixed path at the one binary.
# Exec-form ENTRYPOINT does not expand build args, and a shell script would
# need /bin/sh, which scratch does not have.
RUN --mount=type=cache,target=/root/.cache --mount=type=cache,target=/nix,from=docker.io/nixos/nix:latest,source=/nix \
	set -eu; \
	nix build .#"${package}" --out-link result; \
	out=/output/root; \
	mkdir -p "$out/nix/store" "$out/tmp"; \
	chmod 1777 "$out/tmp"; \
	touch "$out/tmp/.keep"; \
	cp -a $(nix-store -qR result) "$out/nix/store/"; \
	bins=$(ls -1 result/bin 2>/dev/null || true); \
	count=$(printf '%s\n' "$bins" | grep -c . || true); \
	if [ "$count" -ne 1 ]; then \
		printf '%s\n' "error: ${package} produced ${count} binaries, expected 1" >&2; \
		exit 1; \
	fi; \
	ln -s "$(readlink -f result)/bin/${bins}" "$out/entrypoint"; \
	bundle=$(find "$out/nix/store" -type f -name ca-bundle.crt -print -quit); \
	if [ -z "$bundle" ]; then \
		printf '%s\n' "error: ${package} closure has no CA bundle; outbound TLS would fail" >&2; \
		exit 1; \
	fi; \
	rel=${bundle#"$out/nix/store/"}; \
	mkdir -p "$out/etc/ssl/certs"; \
	ln -s "/nix/store/${rel}" "$out/etc/ssl/certs/ca-certificates.crt"

FROM scratch
COPY --from=builder /output/root/ /
ENTRYPOINT ["/entrypoint"]
