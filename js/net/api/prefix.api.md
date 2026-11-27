# `@moq/net/prefix`

```ts
export namespace InvalidPrefix {
	export type Code = "wildcard" | "path";
}

export class InvalidPrefix extends Error {
	constructor(code: InvalidPrefix.Code, message: string, options?: ErrorOptions);
	readonly code: InvalidPrefix.Code;
}

export class Prefix {
	static all(): Prefix;
	static compare(a: Prefix, b: Prefix): number;
	equals(other: Prefix): boolean;
	get isEmpty(): boolean;
	matches(path: string): boolean;
	overlaps(path: string): boolean;
	static parse(text: string): Prefix;
	rebase(root: string): Prefix | undefined;
	rooted(root: string): Prefix;
	readonly segments: readonly string[];
	readonly text: string;
	toJSON(): string;
	toString(): string;
}

export class Prefixes implements Iterable<Prefix> {
	constructor(prefixes?: Iterable<Prefix>);
	[Symbol.iterator](): Iterator<Prefix>;
	static all(): Prefixes;
	covers(other: Prefixes): boolean;
	equals(other: Prefixes): boolean;
	insert(prefix: Prefix): boolean;
	intersect(other: Prefixes): Prefixes;
	matches(path: string): boolean;
	overlaps(path: string): boolean;
	static parse(texts: Iterable<string>): Prefixes;
	rebase(root: string): Prefixes;
	rooted(root: string): Prefixes;
	get size(): number;
	toArray(): Prefix[];
	toJSON(): string[];
}

export function escape(content: string): string;

export function parts(path: string, limit?: number): string[];

export function unescape(segment: string): string;
```
