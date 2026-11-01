import * as Moq from "@moq/net";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const origin = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({ url, consume: origin });

	// Keep one publisher instance; wait.ts shows following publisher restarts.
	const broadcast = origin.request(Moq.Path.from("my-broadcast"));

	// Subscribe to a specific track (with the default subscription)
	let active = broadcast.active.peek();
	while (!active) {
		await broadcast.active.changed();
		active = broadcast.active.peek();
	}
	const track = active.track("chat").subscribe();

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
	broadcast.close();
	await connection.close();
	origin.close();
}

main().catch(console.error);
