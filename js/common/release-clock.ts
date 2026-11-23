// A bun preload for release.test.ts: run release.ts on a virtual clock that jumps to each
// deadline, so its registry waits finish at once and the test reads how long they were.
import { Instant } from "@moq/clock";
import { Manual } from "@moq/clock/test";

const clock = new Manual();
clock.install();
process.on("exit", () => console.log(`waited ${clock.now().since(Instant.ZERO).asSecs()}s`));
void (async () => {
	for (;;) await clock.fire();
})();
