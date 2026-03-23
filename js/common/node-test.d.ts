// Ambient module declarations for node:test and node:assert
// needed when using moduleResolution: "bundler" which doesn't resolve node: protocol
declare module "node:test" {
	export default function test(name: string, fn: () => void | Promise<void>): void;
	export function describe(name: string, fn: () => void | Promise<void>): void;
	export function it(name: string, fn: () => void | Promise<void>): void;
	export function before(fn: () => void | Promise<void>): void;
	export function after(fn: () => void | Promise<void>): void;
	export function beforeEach(fn: () => void | Promise<void>): void;
	export function afterEach(fn: () => void | Promise<void>): void;
}

declare module "node:assert" {
	function assert(value: unknown, message?: string | Error): asserts value;
	namespace assert {
		function ok(value: unknown, message?: string | Error): asserts value;
		function strictEqual<T>(actual: unknown, expected: T, message?: string | Error): asserts actual is T;
		function deepEqual(actual: unknown, expected: unknown, message?: string | Error): void;
		function deepStrictEqual(actual: unknown, expected: unknown, message?: string | Error): void;
		function notStrictEqual(actual: unknown, expected: unknown, message?: string | Error): void;
		function notDeepStrictEqual(actual: unknown, expected: unknown, message?: string | Error): void;
		function throws(block: () => unknown, message?: string | Error): void;
		function throws(block: () => unknown, error: Function | RegExp | object, message?: string | Error): void;
		function rejects(
			asyncFn: (() => Promise<unknown>) | Promise<unknown>,
			message?: string | Error,
		): Promise<void>;
		function rejects(
			asyncFn: (() => Promise<unknown>) | Promise<unknown>,
			error: Function | RegExp | object,
			message?: string | Error,
		): Promise<void>;
		function fail(message?: string | Error): never;
	}
	export = assert;
}
