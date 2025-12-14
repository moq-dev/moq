#!/usr/bin/env just --justfile

# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation

set quiet

# List all of the available commands.
default:
  just --list

# Install any dependencies.
install:
	bun install
	cargo install --locked cargo-shear cargo-sort cargo-upgrades cargo-edit

# Alias for dev.
all: dev

# Run the relay, web server, and publish bbb.
dev:
	# Install any JS dependencies.
	bun install

	# Build the rust packages so `cargo run` has a head start.
	cargo build

	# Then run the relay with a slight head start.
	# It doesn't matter if the web beats BBB because we support automatic reloading.
	bun run concurrently --kill-others --names srv,bbb,web --prefix-colors auto \
		"just relay" \
		"sleep 1 && just pub bbb http://localhost:4443/anon" \
		"sleep 2 && just web http://localhost:4443/anon"

# Run a localhost relay server without authentication.
relay:
	# Run the relay server overriding the provided configuration file.
	cargo run --bin moq-relay -- dev/relay.toml

# Run a cluster of relay servers
cluster:
	# Install any JS dependencies.
	bun install

	# Generate auth tokens if needed
	@just auth-token

	# Build the Rust packages so `cargo run` has a head start.
	cargo build --bin moq-relay

	# Then run a BOATLOAD of services to make sure they all work correctly.
	# Publish the funny bunny to the root node.
	# Publish the robot fanfic to the leaf node.
	bun run concurrently --kill-others --names root,leaf,bbb,tos,web --prefix-colors auto \
		"just root" \
		"sleep 1 && just leaf" \
		"sleep 2 && just pub bbb http://localhost:4444/demo?jwt=$(cat dev/demo-cli.jwt)" \
		"sleep 3 && just pub tos http://localhost:4443/demo?jwt=$(cat dev/demo-cli.jwt)" \
		"sleep 4 && just web http://localhost:4443/demo?jwt=$(cat dev/demo-web.jwt)"

# Run a localhost root server, accepting connections from leaf nodes.
root: auth-key
	# Run the root server with a special configuration file.
	cargo run --bin moq-relay -- dev/root.toml

# Run a localhost leaf server, connecting to the root server.
leaf: auth-token
	# Run the leaf server with a special configuration file.
	cargo run --bin moq-relay -- dev/leaf.toml

# Generate a random secret key for authentication.
# By default, this uses HMAC-SHA256, so it's symmetric.
# If some one wants to contribute, public/private key pairs would be nice.
auth-key:
	@if [ ! -f "dev/root.jwk" ]; then \
		rm -f dev/*.jwt; \
		cargo run --bin moq-token -- --key "dev/root.jwk" generate; \
	fi

# Generate authentication tokens for local development
# demo-web.jwt - allows publishing to demo/me/* and subscribing to demo/*
# demo-cli.jwt - allows publishing to demo/* but no subscribing
# root.jwt - allows publishing and subscribing to all paths
auth-token: auth-key
	@if [ ! -f "dev/demo-web.jwt" ]; then \
		cargo run --quiet --bin moq-token -- --key "dev/root.jwk" sign \
			--root "demo" \
			--subscribe "" \
			--publish "me" \
			> dev/demo-web.jwt ; \
	fi

	@if [ ! -f "dev/demo-cli.jwt" ]; then \
		cargo run --quiet --bin moq-token -- --key "dev/root.jwk" sign \
			--root "demo" \
			--publish "" \
			> dev/demo-cli.jwt ; \
	fi

	@if [ ! -f "dev/root.jwt" ]; then \
		cargo run --quiet --bin moq-token -- --key "dev/root.jwk" sign \
			--root "" \
			--subscribe "" \
			--publish "" \
			--cluster \
			> dev/root.jwt ; \
	fi

# Download the video and convert it to a fragmented MP4 that we can stream
download name:
	@if [ ! -f "dev/{{name}}.mp4" ]; then \
		curl -fsSL $(just download-url {{name}}) -o "dev/{{name}}.mp4"; \
	fi

	@if [ ! -f "dev/{{name}}.fmp4" ]; then \
		ffmpeg -loglevel error -i "dev/{{name}}.mp4" \
			-c:v copy \
			-f mp4 -movflags cmaf+separate_moof+delay_moov+skip_trailer+frag_every_frame \
			"dev/{{name}}.fmp4"; \
	fi

# Returns the URL for a test video.
download-url name:
	@case {{name}} in \
		bbb) echo "http://commondatastorage.googleapis.com/gtv-videos-bucket/sample/BigBuckBunny.mp4" ;; \
		tos) echo "http://commondatastorage.googleapis.com/gtv-videos-bucket/sample/TearsOfSteel.mp4" ;; \
		av1) echo "http://download.opencontent.netflix.com.s3.amazonaws.com/AV1/Sparks/Sparks-5994fps-AV1-10bit-1920x1080-2194kbps.mp4" ;; \
		hevc) echo "https://test-videos.co.uk/vids/jellyfish/mp4/h265/1080/Jellyfish_1080_10s_30MB.mp4" ;; \
		*) echo "unknown" && exit 1 ;; \
	esac

# Publish a video using ffmpeg to the localhost relay server
# NOTE: The `http` means that we perform insecure certificate verification.
# Switch it to `https` when you're ready to use a real certificate.
pub name url="http://localhost:4443/anon" *args:
	# Download the sample media.
	just download "{{name}}"

	# Pre-build the binary so we don't queue media while compiling.
	cargo build --bin hang

	# Run ffmpeg and pipe the output to hang
	ffmpeg -hide_banner -v quiet \
		-stream_loop -1 -re \
		-i "dev/{{name}}.fmp4" \
		-c copy \
		-f mp4 -movflags cmaf+separate_moof+delay_moov+skip_trailer+frag_every_frame \
		- | cargo run --bin hang -- publish --url "{{url}}" --name "{{name}}" {{args}}


# Publish a video using H.264 Annex B format to the localhost relay server
pub-h264 name url="http://localhost:4443/anon" *args:
	# Download the sample media.
	just download "{{name}}"

	# Pre-build the binary so we don't queue media while compiling.
	cargo build --bin hang

	# Run ffmpeg and pipe H.264 Annex B output to hang
	ffmpeg -hide_banner -v quiet \
		-stream_loop -1 -re \
		-i "dev/{{name}}.fmp4" \
		-c:v copy -an \
		-bsf:v h264_mp4toannexb \
		-f h264 \
		- | cargo run --bin hang -- publish --url "{{url}}" --name "{{name}}" --format annex-b {{args}}

# Publish/subscribe using gstreamer - see https://github.com/moq-dev/gstreamer
pub-gst name url='http://localhost:4443/anon':
	@echo "GStreamer plugin has moved to: https://github.com/moq-dev/gstreamer"
	@echo "Install and use hang-gst directly for GStreamer functionality"

# Subscribe to a video using gstreamer - see https://github.com/moq-dev/gstreamer
sub name url='http://localhost:4443/anon':
	@echo "GStreamer plugin has moved to: https://github.com/moq-dev/gstreamer"
	@echo "Install and use hang-gst directly for GStreamer functionality"

# Publish a video using ffmpeg directly from hang to the localhost
serve name:
	# Download the sample media.
	just download "{{name}}"

	# Pre-build the binary so we don't queue media while compiling.
	cargo build --bin hang

	# Run ffmpeg and pipe the output to hang
	ffmpeg -hide_banner -v quiet \
		-stream_loop -1 -re \
		-i "dev/{{name}}.fmp4" \
		-c copy \
		-f mp4 -movflags cmaf+separate_moof+delay_moov+skip_trailer+frag_every_frame \
		- | cargo run --bin hang -- serve --listen "[::]:4443" --tls-generate "localhost" --name "{{name}}"

# Run the web server
web url='http://localhost:4443/anon':
	VITE_RELAY_URL="{{url}}" bun run --filter='*' dev

# Publish the clock broadcast
# `action` is either `publish` or `subscribe`
clock action url="http://localhost:4443/anon" *args:
	@if [ "{{action}}" != "publish" ] && [ "{{action}}" != "subscribe" ]; then \
		echo "Error: action must be 'publish' or 'subscribe', got '{{action}}'" >&2; \
		exit 1; \
	fi

	cargo run --bin moq-clock -- --url "{{url}}" --broadcast "clock" {{args}} {{action}}

# Run the CI checks
check:
	#!/usr/bin/env bash
	set -euo pipefail

	# Run the Javascript checks.
	bun install --frozen-lockfile
	if tty -s; then
		bun run --filter='*' --elide-lines=0 check
	else
		bun run --filter='*' check
	fi
	bun biome check

	# Run the (slower) Rust checks.
	cargo check --all-targets --all-features
	cargo clippy --all-targets --all-features -- -D warnings
	cargo fmt --all --check

	# requires: cargo install cargo-shear
	cargo shear

	# requires: cargo install cargo-sort
	cargo sort --workspace --check

	# Only run the tofu checks if tofu is installed.
	if command -v tofu &> /dev/null; then (cd cdn && just check); fi

	# Only run the nix checks if nix is installed.
	if command -v nix &> /dev/null; then nix flake check; fi


# Run the unit tests
test:
	#!/usr/bin/env bash
	set -euo pipefail

	# Run the Javascript tests.
	bun install --frozen-lockfile
	if tty -s; then
		bun run --filter='*' --elide-lines=0 test
	else
		bun run --filter='*' test
	fi

	cargo test --all-targets --all-features

# Automatically fix some issues.
fix:
	# Fix the Javascript dependencies.
	bun install
	bun biome check --write

	# Fix the Rust issues.
	cargo clippy --fix --allow-staged --allow-dirty --all-targets --all-features
	cargo fmt --all

	# requires: cargo install cargo-shear
	cargo shear --fix

	# requires: cargo install cargo-sort
	cargo sort --workspace

	if command -v tofu &> /dev/null; then (cd cdn && just fix); fi

# Upgrade any tooling
update:
	bun update
	bun outdated

	# Update any patch versions
	cargo update

	# Requires: cargo install cargo-upgrades cargo-edit
	cargo upgrade --incompatible

	# Update the Nix flake.
	nix flake update

# Build the packages
build:
	bun run --filter='*' build
	cargo build
