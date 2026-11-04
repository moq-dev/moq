/** Per-session socket handshake, frame delivery and teardown swept over endpoints x sessions per endpoint. */
import { yieldNow } from "@moq/clock";
import { Origin, Path, socket } from "../src/index.ts";

console.log("protocol,endpoints,sessions_per_endpoint,open_us,frame_us,close_us");
for (const protocol of ["tcp", "track"] as const)
	for (const endpoints of [1, 8, 32])
		for (const sessions of [1, 8, 32]) {
			const samples: number[][] = [];
			for (let round = 0; round < 4; round++) {
				const origin = new Origin.Producer();
				const dialer = new socket.Endpoint(origin, Path.from("bench"), "dialer");
				const peers = Array.from(
					{ length: endpoints },
					(_, i) => new socket.Endpoint(origin, Path.from("bench"), `peer-${i}`),
				);
				const start = performance.now();
				const pairs = await Promise.all(
					peers.flatMap((peer) =>
						Array.from({ length: sessions }, async () => {
							const incoming = protocol === "tcp" ? peer.acceptTcp() : peer.acceptTrack();
							const dialing = protocol === "tcp" ? dialer.dialTcp(peer.id) : dialer.dialTrack(peer.id);
							return Promise.all([dialing, (await incoming).accept()]);
						}),
					),
				);
				const opened = performance.now();
				await Promise.all(
					pairs.map(async ([local, remote]) => {
						local.producer.writeFrame({ payload: new Uint8Array(1024) });
						const frame =
							remote instanceof socket.TcpStream
								? await remote.consumer.readFrame()
								: await remote.consumer.ordered().readFrame();
						if (frame?.payload.length !== 1024) throw new Error("missing socket payload");
					}),
				);
				const received = performance.now();
				dialer.close();
				for (const peer of peers) peer.close();
				origin.close();
				await yieldNow();
				const closed = performance.now();
				if (round)
					samples.push(
						[opened - start, received - opened, closed - received].map((ms) => (ms * 1000) / pairs.length),
					);
			}
			const median = [0, 1, 2].map((i) =>
				samples
					.map((sample) => sample[i])
					.sort((a, b) => a - b)[1]
					.toFixed(2),
			);
			console.log(`${protocol},${endpoints},${sessions},${median.join(",")}`);
		}
