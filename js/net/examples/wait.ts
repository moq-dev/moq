import * as Moq from "@moq/net";

// @moq/net re-exports the reactive primitives, so an app only needs the one dependency.
const { Effect } = Moq.Signals;

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const connection = new Moq.Connection({ url });

	// Wait for a broadcast that may not exist yet. `consume` would subscribe blind and get reset
	// if nobody is publishing the path; this waits for the announcement instead.
	const broadcast = connection.announcedBroadcast(Moq.Path.from("my-broadcast"));

	const effect = new Effect();
	effect.run((effect) => {
		// Re-runs every time the broadcast comes online or goes away, including across reconnects
		// and same-name republishes.
		const active = effect.get(broadcast.active);
		if (!active) {
			console.log("broadcast is offline");
			return;
		}

		console.log("broadcast is live");
		const track = active.track("chat").subscribe({ priority: 0 });
		effect.cleanup(() => track.close());

		effect.spawn(async () => {
			for (;;) {
				const group = await Promise.race([effect.cancel, track.recvGroup()]);
				if (!group) break;
				console.log("received:", await group.readString());
			}
		});
	});

	// Attempt failures land on `error` instead of settling `closed`, so a JWT refresh
	// can recover the same handle. This example logs and closes; replace `url` instead
	// if the credentials can be renewed.
	effect.run((effect) => {
		const err = effect.get(connection.error);
		if (!err) return;
		console.error("connection failed:", err);
		connection.close(err);
	});

	try {
		await connection.closed;
	} finally {
		effect.close();
		broadcast.close();
		connection.close();
	}
}

main().catch(console.error);
