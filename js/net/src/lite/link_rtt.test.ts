import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { LinkRtt } from "./link_rtt.ts";

// Mirrors the tests of `route/rtt.rs` in rs/moq-net.

const ms = (n: number) => Duration.millis(n);
const asMs = (rtt: Duration | undefined) => rtt?.asMillis();
const feed = (link: LinkRtt, reads: (Duration | undefined)[]) => reads.map((rtt) => asMs(link.read(rtt)));

test("the first read reports at once", () => {
	expect(asMs(new LinkRtt().read(ms(30)))).toBe(30);
});

test("a queue shorter than the window reports nothing", () => {
	const link = new LinkRtt();
	link.read(ms(30));
	const queued = Array.from({ length: 29 }, (_, i) => ms(30 + (i + 1) * 3));
	expect(feed(link, queued).every((report) => report === 30)).toBe(true);
	expect(asMs(link.read(ms(31)))).toBe(30);
});

test("a rise reports once the window passes it", () => {
	const link = new LinkRtt();
	link.read(ms(30));
	const reports = feed(link, Array(30).fill(ms(55)));
	expect(reports.slice(0, 29).every((report) => report === 30)).toBe(true);
	expect(reports[29]).toBe(55);
});

test("a drop reports at once", () => {
	const link = new LinkRtt();
	link.read(ms(80));
	expect(asMs(link.read(ms(40)))).toBe(40);
});

test("a change within the band reports nothing", () => {
	let link = new LinkRtt();
	link.read(ms(10));
	expect(asMs(link.read(ms(5)))).toBe(10);
	expect(asMs(link.read(ms(4)))).toBe(4);
	link = new LinkRtt();
	link.read(ms(100));
	expect(asMs(link.read(ms(75)))).toBe(100);
	expect(asMs(link.read(ms(74)))).toBe(74);
});

test("a transport without rtt reports none and starts over", () => {
	const link = new LinkRtt();
	link.read(ms(80));
	expect(link.read(undefined)).toBeUndefined();
	expect(asMs(link.read(ms(90)))).toBe(90);
});

test("a window without rtt stops reading", () => {
	const link = new LinkRtt();
	feed(link, Array(29).fill(undefined));
	expect(link.reading).toBe(true);
	link.read(undefined);
	expect(link.reading).toBe(false);
});

test("a fraction of a millisecond rounds up", () => {
	expect(asMs(new LinkRtt().read(Duration.micros(300)))).toBe(1);
});
