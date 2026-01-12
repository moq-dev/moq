import * as Moq from "@moq/lite";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const connection = await Moq.Connection.connect(url);

	// Subscribe to a broadcast
	const broadcast = connection.consume(Moq.Path.from("my-broadcast"));

	// Subscribe to a specific track
	const track = broadcast.subscribe({ name: "chat" });

	// Read data as it arrives
	for (;;) {
		const group = await track.nextGroup();
		if (!group) break;

		for (;;) {
			const frame = await group.readFrame();
			if (!frame) break;

			console.log("Received:", frame.toString());
		}
	}

	connection.close();
}

main().catch(console.error);
