import { quicheLoaded, WebTransport } from "@fails-components/webtransport";
import { parseArgs } from "util";

// Polyfill WebTransport for Bun/Node environments
// @ts-ignore - assigning to globalThis
globalThis.WebTransport = WebTransport;

import * as Moq from "@moq/lite";

interface Config {
	url: string;
	broadcast: string;
	track: string;
	role: "publish" | "subscribe";
}

function parseConfig(): Config {
	const { values, positionals } = parseArgs({
		args: process.argv.slice(2),
		options: {
			url: { type: "string" },
			broadcast: { type: "string" },
			track: { type: "string", default: "seconds" },
			help: { type: "boolean", short: "h" },
		},
		allowPositionals: true,
	});

	if (values.help) {
		console.log(`
Usage: bun run main.ts [OPTIONS] <publish|subscribe>
   or: npx tsx main.ts [OPTIONS] <publish|subscribe>

OPTIONS:
    --url <URL>         Connect to the given URL starting with https://
    --broadcast <NAME>  The name of the broadcast to publish or subscribe to
    --track <NAME>      The name of the clock track [default: seconds]
    -h, --help          Print help information

COMMANDS:
    publish     Publish a clock broadcast
    subscribe   Subscribe to a clock broadcast

ENVIRONMENT VARIABLES:
    MOQ_URL     Default URL to connect to
    MOQ_NAME    Default broadcast name
		`);
		process.exit(0);
	}

	const role = positionals[0];
	if (!role || (role !== "publish" && role !== "subscribe")) {
		console.error("Error: Must specify 'publish' or 'subscribe' command");
		process.exit(1);
	}

	const url = values.url || process.env.MOQ_URL;
	const broadcast = values.broadcast || process.env.MOQ_NAME;

	if (!url || !broadcast) {
		console.error("Error: --url and --broadcast are required");
		console.error("Provide them as arguments or set MOQ_URL and MOQ_NAME environment variables");
		process.exit(1);
	}

	return {
		url,
		broadcast,
		track: values.track,
		role: role as "publish" | "subscribe",
	};
}

async function publish(config: Config) {
	const connection = await Moq.Connection.connect(new URL(config.url));
	console.log("✅ Connected to relay:", config.url);

	// Create a new "broadcast", which is a collection of tracks.
	const broadcast = new Moq.Broadcast();
	connection.publish(Moq.Path.from(config.broadcast), broadcast);

	console.log("✅ Published broadcast:", config.broadcast);

	// Wait until we get a subscription for the track
	for (;;) {
		const request = await broadcast.requested();
		if (!request) break;

		if (request.track.name === config.track) {
			publishTrack(request.track);
		} else {
			request.track.close(new Error("not found"));
		}
	}
}

async function publishTrack(track: Moq.Track) {
	// Send timestamps over the wire, matching the Rust implementation format
	console.log("✅ Publishing clock data on track:", track.name);

	for (;;) {
		const now = new Date();

		// Create a new group for each minute (matching Rust implementation)
		const group = track.appendGroup();

		// Send the base timestamp (everything but seconds) - matching Rust format
		const base = `${now.toISOString().slice(0, 16).replace("T", " ")}:`;
		group.writeString(base);

		// Send individual seconds for this minute
		const currentMinute = now.getMinutes();

		while (new Date().getMinutes() === currentMinute) {
			const secondsNow = new Date();
			const seconds = secondsNow.getSeconds().toString().padStart(2, "0");

			group.writeString(seconds);

			// Wait until next second
			const nextSecond = new Date(secondsNow);
			nextSecond.setSeconds(nextSecond.getSeconds() + 1, 0);
			const delay = nextSecond.getTime() - secondsNow.getTime();

			if (delay > 0) {
				await new Promise((resolve) => setTimeout(resolve, delay));
			}

			// Check if we've moved to next minute
			if (new Date().getMinutes() !== currentMinute) {
				break;
			}
		}

		group.close();
	}
}

async function subscribe(config: Config) {
	const connection = await Moq.Connection.connect(new URL(config.url));
	console.log("✅ Connected to relay:", config.url);

	const broadcast = connection.consume(Moq.Path.from(config.broadcast));
	const track = broadcast.subscribe(config.track, 0);

	console.log("✅ Subscribed to track:", config.track);

	// Handle groups and frames like the Rust implementation
	for (;;) {
		const group = await track.nextGroup();
		if (!group) {
			console.log("❌ Connection ended");
			break;
		}

		// Get the base timestamp (first frame in group)
		const baseFrame = await group.readFrame();
		if (!baseFrame) {
			console.warn("❌ No base frame found");
			continue;
		}

		const base = new TextDecoder().decode(baseFrame);

		// Read individual second frames
		for (;;) {
			const frame = await group.readString();
			if (!frame) {
				break; // End of group
			}

			const seconds = parseInt(frame, 10);

			// Clock emoji positions
			const clockEmojis = ["🕛", "🕐", "🕑", "🕒", "🕓", "🕔", "🕕", "🕖", "🕗", "🕘", "🕙", "🕚"];

			// Map 60 seconds to 12 clock positions (5 seconds per position)
			const clockIndex = Math.floor((seconds / 60) * clockEmojis.length) % clockEmojis.length;
			const clockEmoji = clockEmojis[clockIndex];

			console.log(clockEmoji, base + seconds);
		}
	}
}

// Wait for the WebTransport polyfill to be ready
await quicheLoaded;

try {
	const config = parseConfig();

	if (config.role === "publish") {
		await publish(config);
	} else {
		await subscribe(config);
	}
} catch (error) {
	console.error("❌ Error:", error);
	process.exit(1);
}
