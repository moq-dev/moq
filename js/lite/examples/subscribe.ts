import * as Moq from "@moq/lite";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const connection = await Moq.Connection.connect(url);

	// Subscribe to a broadcast
	const broadcast = connection.consume(Moq.Path.from("my-broadcast"));

	// Subscribe to a specific track (with priority 0)
	const track = broadcast.subscribe("chat", 0);

	// Read data as it arrives
	for (;;) {
		const group = await track.nextGroup();
		if (!group) break;

		for (;;) {
			const frame = await group.readString();
			if (!frame) break;

			console.log("Received:", frame);
		}
	}

	connection.close();
}

main().catch(console.error);
