# We're using a Dockerfile despite the fact that Nix can create Docker images directly.
#
# 1. It's difficult to cross compile Docker images with Nix.
#   - I tried, but OSX makes it even more difficult.
# 2. Nix is not required for developers; `docker build .` will work.
#
# Unfortunately, it means that caching is more difficult.
# Nix uses /nix/store for both caching AND the final output (lots of symlinks)
FROM nixos/nix:latest AS builder
ENV NIX_CONFIG="experimental-features = nix-command flakes"

WORKDIR /build

RUN mkdir -p /output/store

COPY . .

# Build stage that accepts an optional package argument
ARG package

# Build the package
RUN --mount=type=cache,target=/root/.cache --mount=type=cache,target=/nix,from=nixos/nix:latest,source=/nix \
	nix build .#${package} --out-link result && \
	cp -r $(nix-store -qR result) /output/store && \
	cp -r $(readlink -f result) /output/result && \
	rm -rf /output/store/$(basename $(readlink -f result))

# Default to `/bin/sh` for the entrypoint if no package is specified
ARG package="sh"

# Create entry.sh script that knows which binary to run
RUN echo '#!/bin/sh' > /output/entry.sh && \
	echo "exec /bin/${package} \"\$@\"" >> /output/entry.sh && \
	chmod +x /output/entry.sh

# Final image (when no specific package is selected, defaults to sh)
FROM nixos/nix:latest

COPY --from=builder /output/entry.sh /bin/entry.sh
COPY --from=builder /output/store /nix/store
COPY --from=builder /output/result/bin/* /bin/

ENTRYPOINT ["/bin/entry.sh"]
