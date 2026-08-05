import { expect, spyOn, test } from "bun:test";
import * as Announce from "./announced.ts";
import { resetNoDiscoveryWarnings } from "./announced.ts";
import * as Path from "./path.ts";

const p = (s: string) => Path.from(s);

test("next streams every appended event in order", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	producer.append({ path: p("a"), active: true });
	producer.append({ path: p("a"), active: false });

	expect(await consumer.next()).toEqual({ path: p("a"), active: true });
	expect(await consumer.next()).toEqual({ path: p("a"), active: false });
});

test("a same-name re-announce is a distinct update", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	// The stream is a log, not a set: it carries a redundant active:true as its own update rather
	// than collapsing it. Deciding what a repeat means belongs to the session layer, which resolves
	// a restart into either nothing (a route change) or an end + start (a new publisher).
	producer.append({ path: p("a"), active: true });
	producer.append({ path: p("a"), active: true });

	expect(await consumer.next()).toEqual({ path: p("a"), active: true });
	expect(await consumer.next()).toEqual({ path: p("a"), active: true });
});

test("closing resolves next with undefined", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	producer.close();
	expect(await consumer.next()).toBeUndefined();
});

test("aborting rejects next", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	producer.close(new Error("boom"));
	await expect(consumer.next()).rejects.toThrow("boom");
});

// A stub session with no discovery, so Broadcast takes the warn-and-consume-blind path.
function noDiscovery(url: string): Announce.BroadcastProps["connection"] {
	return {
		url: new URL(url),
		discovery: false,
		closed: new Promise<void>(() => {}),
		consume: () => ({ close() {}, closed: { peek: () => undefined } }),
	} as unknown as Announce.BroadcastProps["connection"];
}

// Count what the handles log on their first run, so the warning is measured rather than
// eyeballed. The handles are closed only after that run, since closing one in the same job
// tears its effect down before it ever warns.
async function countWarnings(fn: () => Announce.Broadcast[]): Promise<number> {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const handles = fn();

	try {
		await new Promise((resolve) => setTimeout(resolve, 0));
		return warn.mock.calls.length;
	} finally {
		for (const handle of handles) handle.close();
		warn.mockRestore();
	}
}

test("the no-discovery warning is once per relay, ignoring the auth token", async () => {
	resetNoDiscoveryWarnings();

	// Same relay, different tokens and different watched paths: still one relay.
	const warnings = await countWarnings(() => [
		new Announce.Broadcast({ connection: noDiscovery("https://relay.example/anon?jwt=a"), path: p("one") }),
		new Announce.Broadcast({ connection: noDiscovery("https://relay.example/anon?jwt=b"), path: p("two") }),
		new Announce.Broadcast({ connection: noDiscovery("https://other.example/anon"), path: p("one") }),
	]);

	expect(warnings).toBe(2);
});

test("the no-discovery warning cache is bounded", async () => {
	resetNoDiscoveryWarnings();

	// Fill past the cap, then come back to the very first relay. An unbounded cache would
	// still remember it and stay silent; a bounded one has evicted it and warns again.
	const first = "https://relay0.example/anon";
	await countWarnings(() =>
		Array.from(
			{ length: 200 },
			(_, i) =>
				new Announce.Broadcast({ connection: noDiscovery(`https://relay${i}.example/anon`), path: p("x") }),
		),
	);

	const warnings = await countWarnings(() => [
		new Announce.Broadcast({ connection: noDiscovery(first), path: p("x") }),
	]);

	expect(warnings).toBe(1);
});
