declare module "bun:test" {
	export function describe(name: string, fn: () => void): void;
	export function it(name: string, fn: () => void | Promise<void>): void;
	export function test(name: string, fn: () => void | Promise<void>): void;
	interface Matchers<T> {
		toBe(expected: T): void;
		toEqual(expected: T): void;
		toBeLessThan(expected: number): void;
		toBeGreaterThan(expected: number): void;
		toBeCloseTo(expected: number, precision?: number): void;
		toThrow(expected?: string | RegExp | Error): void;
		not: Matchers<T>;
	}
	export function expect<T>(value: T): Matchers<T>;
	export function beforeEach(fn: () => void | Promise<void>): void;
	export function afterEach(fn: () => void | Promise<void>): void;
	export function beforeAll(fn: () => void | Promise<void>): void;
	export function afterAll(fn: () => void | Promise<void>): void;
}
