import { expect, mock, test } from "bun:test";
import { SessionCode } from "../error.ts";
import { SlotMap, Slots } from "./limits.ts";

test("a full budget closes the session, and released slots can be used again", () => {
	const close = mock(() => {});
	const slots = new Slots(2, { quic: { close } as unknown as WebTransport, reason: "too many tests" });
	slots.acquire();
	slots.acquire();
	slots.release();
	slots.acquire();
	expect(() => slots.acquire()).toThrow("too many tests");
	expect(close).toHaveBeenCalledWith({ closeCode: SessionCode.TooManyRequests, reason: "too many tests" });
});

test("a slot map charges each key once and releases on delete and clear", () => {
	const map = new SlotMap<string, number>(new Slots(2, { reason: "full" }));
	map.set("a", 1);
	map.set("a", 2);
	map.set("b", 1);
	expect(() => map.set("c", 1)).toThrow("full");
	expect(map.has("c")).toBe(false);
	map.delete("a");
	map.delete("a");
	map.set("c", 1);
	map.clear();
	map.set("d", 1);
	map.set("e", 1);
});

test("tryAcquire refuses at the cap without closing the session", () => {
	const slots = new Slots(1, { reason: "full" });
	expect(slots.tryAcquire()).toBe(true);
	expect(slots.tryAcquire()).toBe(false);
	slots.release();
	expect(slots.tryAcquire()).toBe(true);
	// A held entry counts past the cap, so tryAcquire waits for it too.
	slots.hold();
	slots.release();
	expect(slots.tryAcquire()).toBe(false);
	slots.release();
	expect(slots.tryAcquire()).toBe(true);
});
