import * as Moq from "@moq/net";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const origin = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({ url, consume: origin });

	// Keep one publisher identity, or throw if none serves the path; wait.ts shows following
	// publisher restarts. The request owns the broadcast, so close it when done.
	const request = origin.request(Moq.Path.from("my-broadcast"));
	const broadcast = await request.broadcast();

	// Subscribe to a specific track (with the default subscription)
	const track = broadcast.track("chat").subscribe();

	// Read data as it arrives
	for (;;) {
		const group = await track.recvGroup();
		if (!group) break;

		for (;;) {
			let frame: string | undefined;
			try {
				frame = await group.readString();
			} catch (err) {
				// The publisher reset the group. The code reads the same way on every transport.
				if (err instanceof Moq.Error.Stream) {
					console.warn("group reset with code", err.code);
					break;
				}
				throw err;
			}

			if (!frame) break;
			console.log("Received:", frame);
		}
	}

	track.close();
	request.close();
	await connection.close();
	origin.close();
}

main().catch(console.error);
