/** Per-session socket handshake, frame delivery and teardown swept over endpoints x sessions per endpoint. */
import { yieldNow } from "@moq/clock";
import { accept } from "../src/connection/accept.ts";
import { connect } from "../src/connection/connect.ts";
import { Origin, Path, socket } from "../src/index.ts";
import * as Lite from "../src/lite/index.ts";
import { createMockTransportPair } from "../src/mock.ts";

console.debug = () => {};
console.log("mode,protocol,endpoints,sessions_per_endpoint,open_us,frame_us,close_us");
for (const mode of ["local", "lite07"] as const)
	for (const protocol of ["tcp", "track"] as const)
		for (const endpoints of [1, 8, 32])
			for (const sessions of [1, 8, 32]) {
				const samples: number[][] = [];
				for (let round = 0; round < 4; round++) {
					const origin = new Origin.Producer();
					const remote = mode === "local" ? origin : new Origin.Producer();
					const pair = mode === "lite07" ? createMockTransportPair(Lite.ALPN_07_WIP) : undefined;
					const connections = pair
						? await Promise.all([
								connect({
									url: new URL("https://localhost/bench"),
									transport: pair.client,
									publish: origin.consume(),
									consume: origin,
								}),
								accept({
									url: new URL("https://localhost/bench"),
									transport: pair.server,
									publish: remote.consume(),
									consume: remote,
								}),
							])
						: [];
					const dialer = new socket.Endpoint(origin, Path.from("bench"), "dialer");
					const peers = Array.from(
						{ length: endpoints },
						(_, i) => new socket.Endpoint(remote, Path.from("bench"), `peer-${i}`),
					);
					const start = performance.now();
					const pairs = await Promise.all(
						peers.flatMap((peer) =>
							Array.from({ length: sessions }, async () => {
								const incoming = protocol === "tcp" ? peer.acceptTcp() : peer.acceptTrack();
								const dialing =
									protocol === "tcp" ? dialer.dialTcp(peer.id) : dialer.dialTrack(peer.id);
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
					remote.close();
					for (const connection of connections) connection.abort();
					await yieldNow();
					const closed = performance.now();
					if (round)
						samples.push(
							[opened - start, received - opened, closed - received].map(
								(ms) => (ms * 1000) / pairs.length,
							),
						);
				}
				const median = [0, 1, 2].map((i) =>
					samples
						.map((sample) => sample[i])
						.sort((a, b) => a - b)[1]
						.toFixed(2),
				);
				console.log(`${mode},${protocol},${endpoints},${sessions},${median.join(",")}`);
			}
