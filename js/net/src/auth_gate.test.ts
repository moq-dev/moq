import { expect, test } from "bun:test";
import { Signal } from "@moq/signals";
import type { Grant } from "./auth.ts";
import { Gate } from "./auth_gate.ts";
import * as Path from "./path.ts";

const grant = (prefix: string): Grant => ({
	publish: new Path.Patterns([Path.Pattern.subtree(prefix)]),
	subscribe: new Path.Patterns([]),
});

test("a gate allows everything until the peer answers", () => {
	const gate = new Gate(new Signal<Grant | undefined>(undefined));
	expect(gate.allows(Path.from("room/alice"))).toBe(true);
	expect(new Gate().allows(Path.from("room/alice"))).toBe(true);
});

test("a watch revokes once when the grant stops covering its path", async () => {
	const peer = new Signal<Grant | undefined>(grant("room"));
	const gate = new Gate(peer);
	let revoked = 0;
	const watch = gate.watch(Path.from("room/alice"), () => revoked++);
	peer.set(grant("room/alice"));
	await Promise.resolve();
	expect(revoked).toBe(0);
	peer.set(grant("other"));
	await Promise.resolve();
	peer.set(grant("elsewhere"));
	await Promise.resolve();
	expect(revoked).toBe(1);
	expect(watch.revoked()).toBe(true);
	watch.dispose();
});
