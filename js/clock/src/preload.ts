// A bun test preload: a test fails when a timer it armed fires on the host clock, unless it called hostTime().
import { afterEach, beforeEach } from "bun:test";
import { guard } from "./host.ts";

beforeEach(() => {
	guard.test = { real: false };
});
afterEach(() => {
	guard.test = undefined;
});
