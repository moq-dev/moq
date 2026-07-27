import * as Moq from "@moq/net";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const connection = await Moq.Connection.connect(url);

	// Subscribe to a broadcast
	const broadcast = connection.consume(Moq.Path.from("my-broadcast"));

	// Subscribe to a specific track (with priority 0)
	const track = broadcast.track("chat").subscribe({ priority: 0 });

	// Read data as it arrives
	for (;;) {
		const group = await track.recvGroup();
		if (!group) break;

		for (;;) {
			let frame: string | undefined;
			try {
				frame = await group.readString();
			} catch (err) {
				// The publisher reset the group. The code is whatever it means to that peer,
				// and reads the same way on every transport.
				if (err instanceof Moq.RemoteError) {
					console.warn("group reset with code", err.code);
					break;
				}
				throw err;
			}

			if (!frame) break;
			console.log("Received:", frame);
		}
	}

	connection.close();
}

main().catch(console.error);
