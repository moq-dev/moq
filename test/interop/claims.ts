import assert from "node:assert/strict";
import Session from "@moq/qmux";
import { Producer as Broadcast } from "../../js/net/src/broadcast.ts";
import { connect } from "../../js/net/src/connection/connect.ts";
import { Producer as Origin } from "../../js/net/src/origin.ts";
import * as Path from "../../js/net/src/path.ts";

const url = new URL(process.argv[2]);
const protocol = "moq-lite-07-wip";
const publisher = new Origin();
const claim = publisher.requests(Path.from("claimed"));
const requested = claim.requested();

async function transport() {
	const session = new Session(url, { protocols: [protocol], versions: { [protocol]: null }, requireProtocol: true });
	await session.ready;
	return session;
}

const publishing = await connect({ url, transport: await transport(), publish: publisher.consume() });
let publisherClosed: Error | null | undefined;
void publishing.closed.then((closed) => {
	publisherClosed = closed;
});
try {
	// The claim's instance stays fixed: new objects continue its sequence without changing bytes.
	for (const sequence of [0n, 1n]) {
		const subscriber = new Origin();
		const consuming = await connect({ url, transport: await transport(), consume: subscriber });
		const announcements = subscriber.consume().announced();
		const following = subscriber.follow(Path.from("claimed/job"));
		const broadcast = new Broadcast();
		const track = broadcast.createTrack("data", { maxAge: null, refresh: 1, priority: 1 });
		try {
			for (;;) {
				const event = await announcements.next();
				assert(event, "relay announcements ended before the claim");
				if (event.prefix === "claimed" && event.kind === "start") break;
			}
			let active = following.active.peek();
			while (!active) active = await following.active.changed();
			const received = active.track("data").subscribe({
				delay: { max: null },
				order: "ascending",
				ranges: [{ start: { relative: 0 } }],
			});
			try {
				const { value: request } = await requested.next();
				assert(request, "claim stopped serving requests");
				assert.equal(request.path, Path.from("claimed/job"));
				request.serve(broadcast);
				const wanted = track.demand().wanted;
				while (wanted.peek() === undefined) await wanted.changed();
				const group = track.createGroup(sequence);
				group.writeFrame({ payload: new Uint8Array([Number(sequence)]) });
				group.finish();
				for (;;) {
					const group = await received.recvGroup();
					assert(group, "claim answer ended before its new group");
					try {
						assert.deepEqual((await group.readFrame())?.payload, new Uint8Array([Number(group.sequence)]));
					} finally {
						group.close();
					}
					if (group.sequence === sequence) break;
					// The relay can replay group 0 before it reads the new source's group 1.
					assert(group.sequence < sequence);
				}
				assert.equal(publisherClosed, undefined, "publisher session closed while re-serving the claim");
			} finally {
				received.close();
			}
		} finally {
			track.close();
			broadcast.close();
			following.close();
			announcements.close();
			await consuming.close();
			subscriber.close();
		}
	}
	await publishing.close();
	assert.equal(await publishing.closed, null, "publisher ended with a session error during claim teardown");
} finally {
	claim.close();
	await requested.return?.(undefined);
	await publishing.close();
	publisher.close();
}
