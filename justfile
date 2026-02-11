#!/usr/bin/env just --justfile

# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation

set quiet

# List all of the available commands.
default:
  just --list

# Install any dependencies.
install:
	bun install
	cargo install --locked cargo-shear cargo-sort cargo-upgrades cargo-edit cargo-hack cargo-sweep

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
relay *args:
	# Run the relay server overriding the provided configuration file.
	TOKIO_CONSOLE_BIND=127.0.0.1:6680 cargo run --bin moq-relay -- dev/relay.toml {{args}}

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
	bun run concurrently --kill-others --names root,leaf0,leaf1,bbb,tos,web --prefix-colors auto \
		"just root" \
		"sleep 1 && just leaf0" \
		"sleep 2 && just leaf1" \
		"sleep 3 && just pub bbb http://localhost:4444/demo?jwt=$(cat dev/demo-cli.jwt)" \
		"sleep 4 && just pub tos http://localhost:4443/demo?jwt=$(cat dev/demo-cli.jwt)" \
		"sleep 5 && just web http://localhost:4445/demo?jwt=$(cat dev/demo-web.jwt)"

# Run a localhost root server, accepting connections from leaf nodes.
root: auth-key
	# Run the root server with a special configuration file.
	cargo run --bin moq-relay -- dev/root.toml

# Run a localhost leaf, connecting to the root server.
leaf0: auth-token
	# Run the leaf server with a special configuration file.
	cargo run --bin moq-relay -- dev/leaf0.toml

# Run a second localhost leaf, connecting to the root server.
leaf1: auth-token
	# Run the leaf server with a special configuration file.
	cargo run --bin moq-relay -- dev/leaf1.toml

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

# Convert an h264 input file to CMAF (fmp4) format to stdout.
ffmpeg-cmaf input output='-' *args:
	ffmpeg -hide_banner -v quiet \
		-stream_loop -1 -re \
		-i "{{input}}" \
		-c copy \
		-f mp4 -movflags cmaf+separate_moof+delay_moov+skip_trailer+frag_every_frame {{args}} {{output}}

# Publish a video using ffmpeg to the localhost relay server
# NOTE: The `http` means that we perform insecure certificate verification.
# Switch it to `https` when you're ready to use a real certificate.
pub name url="http://localhost:4443/anon" *args:
	# Download the sample media.
	just download "{{name}}"
	# Pre-build the binary so we don't queue media while compiling.
	cargo build --bin moq
	# Publish the media with the moq cli.
	just ffmpeg-cmaf "dev/{{name}}.fmp4" |\
	cargo run --bin moq -- \
		{{args}} publish --url "{{url}}" --name "{{name}}" fmp4

pub-iroh name url prefix="":
	# Download the sample media.
	just download "{{name}}"
	# Pre-build the binary so we don't queue media while compiling.
	cargo build --bin moq
	# Publish the media with the moq cli.
	just ffmpeg-cmaf "dev/{{name}}.fmp4" |\
	cargo run --bin moq -- \
		--iroh-enabled publish --url "{{url}}" --name "{{prefix}}{{name}}" fmp4

# Generate and ingest an HLS stream from a video file.
pub-hls name relay="http://localhost:4443/anon":
	#!/usr/bin/env bash
	set -euo pipefail

	just download "{{name}}"

	INPUT="dev/{{name}}.mp4"
	OUT_DIR="dev/{{name}}"

	rm -rf "$OUT_DIR"
	mkdir -p "$OUT_DIR"

	echo ">>> Generating HLS stream to disk (1280x720 + 256x144)..."

	# Start ffmpeg in the background to generate HLS
	ffmpeg -hide_banner -loglevel warning -re -stream_loop -1 -i "$INPUT" \
		-filter_complex "\
		[0:v]split=2[v0][v1]; \
		[v0]scale=-2:720[v720]; \
		[v1]scale=-2:144[v144]" \
		-map "[v720]" -map "[v144]" -map 0:a:0 \
		-r 25 -preset veryfast -g 50 -keyint_min 50 -sc_threshold 0 \
		-c:v:0 libx264 -profile:v:0 high -level:v:0 4.1 -pix_fmt:v:0 yuv420p -tag:v:0 avc1 \
		-b:v:0 4M -maxrate:v:0 4.4M -bufsize:v:0 8M \
		-c:v:1 libx264 -profile:v:1 high -level:v:1 4.1 -pix_fmt:v:1 yuv420p -tag:v:1 avc1 \
		-b:v:1 300k -maxrate:v:1 330k -bufsize:v:1 600k \
		-c:a aac -b:a 128k \
		-f hls -hls_time 2 -hls_list_size 6 \
		-hls_flags independent_segments+delete_segments \
		-hls_segment_type fmp4 \
		-master_pl_name master.m3u8 \
		-var_stream_map "v:0,agroup:audio,name:720 v:1,agroup:audio,name:144 a:0,agroup:audio,name:audio" \
		-hls_segment_filename "$OUT_DIR/v%v/segment_%09d.m4s" \
		"$OUT_DIR/v%v/stream.m3u8" &


	FFMPEG_PID=$!

	# Wait for master playlist to be generated
	echo ">>> Waiting for HLS playlist generation..."
	for i in {1..30}; do
		if [ -f "$OUT_DIR/master.m3u8" ]; then
			break
		fi
		sleep 0.5
	done

	if [ ! -f "$OUT_DIR/master.m3u8" ]; then
		kill $FFMPEG_PID 2>/dev/null || true
		echo "Error: master.m3u8 not generated in time"
		exit 1
	fi

	# Wait for individual playlists to be generated (they're referenced in master.m3u8)
	# Give ffmpeg a bit more time to generate the variant playlists
	echo ">>> Waiting for variant playlists..."
	sleep 2
	for i in {1..20}; do
		# Check if at least one variant playlist exists
		if [ -f "$OUT_DIR/v0/stream.m3u8" ] || [ -f "$OUT_DIR/v720/stream.m3u8" ] || [ -f "$OUT_DIR/v144/stream.m3u8" ] || [ -f "$OUT_DIR/vaudio/stream.m3u8" ]; then
			break
		fi
		sleep 0.5
	done

	# Trap to clean up ffmpeg on exit
	CLEANUP_CALLED=false
	cleanup() {
		if [ "$CLEANUP_CALLED" = "true" ]; then
			return
		fi
		CLEANUP_CALLED=true
		echo "Shutting down..."
		kill $FFMPEG_PID 2>/dev/null || true
		# Wait a bit for ffmpeg to finish
		sleep 0.5
		# Force kill if still running
		kill -9 $FFMPEG_PID 2>/dev/null || true
	}
	trap cleanup SIGINT SIGTERM EXIT

	# Run moq to ingest from local files
	echo ">>> Running with --passthrough flag"
	cargo run --bin moq -- publish --url "{{relay}}" --name "{{name}}" hls --playlist "$OUT_DIR/master.m3u8" --passthrough
	EXIT_CODE=$?

	# Cleanup after cargo run completes (success or failure)
	cleanup

	# Exit with the same code as cargo run
	exit $EXIT_CODE

# Publish a video using H.264 Annex B format to the localhost relay server
pub-h264 name url="http://localhost:4443/anon" *args:
	# Download the sample media.
	just download "{{name}}"

	# Pre-build the binary so we don't queue media while compiling.
	cargo build --bin moq

	# Run ffmpeg and pipe H.264 Annex B output to moq
	ffmpeg -hide_banner -v quiet \
		-stream_loop -1 -re \
		-i "dev/{{name}}.fmp4" \
		-c:v copy -an \
		-bsf:v h264_mp4toannexb \
		-f h264 \
		- | cargo run --bin moq -- publish --url "{{url}}" --name "{{name}}" --format annex-b {{args}}

# Publish/subscribe using gstreamer - see https://github.com/moq-dev/gstreamer
pub-gst name url='http://localhost:4443/anon':
	@echo "GStreamer plugin has moved to: https://github.com/moq-dev/gstreamer"
	@echo "Install and use hang-gst directly for GStreamer functionality"

# Subscribe to a video using gstreamer - see https://github.com/moq-dev/gstreamer
sub name url='http://localhost:4443/anon':
	@echo "GStreamer plugin has moved to: https://github.com/moq-dev/gstreamer"
	@echo "Install and use hang-gst directly for GStreamer functionality"

# Publish a video using ffmpeg directly from moq to the localhost
# To also serve via iroh, pass --iroh-enabled as last argument.
serve name *args:
	# Download the sample media.
	just download "{{name}}"

	# Pre-build the binary so we don't queue media while compiling.
	cargo build --bin moq

	# Run ffmpeg and pipe the output to moq
	just ffmpeg-cmaf "dev/{{name}}.fmp4" |\
	cargo run --bin moq -- \
		{{args}} serve --listen "[::]:4443" --tls-generate "localhost" \
		--name "{{name}}" fmp4

# Run the web server
web url='http://localhost:4443/anon':
	cd js/hang-demo && VITE_RELAY_URL="{{url}}" bun run dev

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

	# Check documentation warnings (only workspace crates, not dependencies)
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

	# requires: cargo install cargo-shear
	cargo shear

	# requires: cargo install cargo-sort
	cargo sort --workspace --check

	# Only run the tofu checks if tofu is installed.
	if command -v tofu &> /dev/null; then (cd cdn && just check); fi

	# Only run the nix checks if nix is installed.
	if command -v nix &> /dev/null; then nix flake check; fi

# Run comprehensive CI checks including all feature combinations (requires cargo-hack)
ci:
	#!/usr/bin/env bash
	set -euo pipefail

	# Run the standard checks first
	just check

	# Run the unit tests
	just test

	# Check all feature combinations for all crates
	# requires: cargo install cargo-hack
	echo "Checking all feature combinations..."
	cargo hack check --workspace --each-feature --no-dev-deps

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

	# Remove old build artifacts to save disk space.
	if command -v cargo-sweep &> /dev/null; then cargo sweep --time 7; fi

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

# Generate and serve an HLS stream from a video for testing pub-hls
serve-hls name port="8000":
	#!/usr/bin/env bash
	set -euo pipefail

	just download "{{name}}"

	INPUT="dev/{{name}}.mp4"
	OUT_DIR="dev/{{name}}"

	rm -rf "$OUT_DIR"
	mkdir -p "$OUT_DIR"

	echo ">>> Starting HLS stream generation..."
	echo ">>> Master playlist: http://localhost:{{port}}/master.m3u8"

	cleanup() {
		echo "Shutting down..."
		kill $(jobs -p) 2>/dev/null || true
		exit 0
	}
	trap cleanup SIGINT SIGTERM

	ffmpeg -loglevel warning -re -stream_loop -1 -i "$INPUT" \
		-map 0:v:0 -map 0:v:0 -map 0:a:0 \
		-r 25 -preset veryfast -g 50 -keyint_min 50 -sc_threshold 0 \
		-c:v:0 libx264 -profile:v:0 high -level:v:0 4.1 -pix_fmt:v:0 yuv420p -tag:v:0 avc1 -bsf:v:0 dump_extra -b:v:0 4M -vf:0 "scale=1920:-2" \
		-c:v:1 libx264 -profile:v:1 high -level:v:1 4.1 -pix_fmt:v:1 yuv420p -tag:v:1 avc1 -bsf:v:1 dump_extra -b:v:1 300k -vf:1 "scale=256:-2" \
		-c:a aac -b:a 128k \
		-f hls \
		-hls_time 2 -hls_list_size 12 \
		-hls_flags independent_segments+delete_segments \
		-hls_segment_type fmp4 \
		-master_pl_name master.m3u8 \
		-var_stream_map "v:0,agroup:audio v:1,agroup:audio a:0,agroup:audio" \
		-hls_segment_filename "$OUT_DIR/v%v/segment_%09d.m4s" \
		"$OUT_DIR/v%v/stream.m3u8" &

	sleep 2
	echo ">>> HTTP server: http://localhost:{{port}}/"
	cd "$OUT_DIR" && python3 -m http.server {{port}}

# Connect tokio-console to the relay server (port 6680)
relay-console:
	tokio-console http://127.0.0.1:6680

# Connect tokio-console to the publisher (port 6681)
pub-console:
	tokio-console http://127.0.0.1:6681

# Serve the documentation locally.
doc:
	cd doc && bun run dev

# Throttle UDP traffic for testing (macOS only, requires sudo)
throttle:
	dev/throttle
