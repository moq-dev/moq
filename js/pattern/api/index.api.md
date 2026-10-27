# `@moq/pattern`

```ts
export class IntersectionError extends Error {
	constructor();
}

export namespace InvalidPattern {
	export type Code = "empty-segment" | "invalid-segment" | "multiple-globstars" | "too-many-segments";
}

export class InvalidPattern extends Error {
	constructor(code: InvalidPattern.Code, message: string);
	readonly code: InvalidPattern.Code;
}

export class Pattern {
	static get MAX_INTERSECTIONS(): number;
	static get MAX_SEGMENTS(): number;
	static all(): Pattern;
	asPrefix(): string | undefined;
	captures(matched: Pattern): Pattern[] | undefined;
	static compare(a: Pattern, b: Pattern): number;
	contains(other: Pattern): boolean;
	static empty(): Pattern;
	equals(other: Pattern): boolean;
	static from(segments: Iterable<Segment>): Pattern;
	get hasGlobstar(): boolean;
	get head(): string;
	intersect(other: Pattern): Patterns;
	get isLiteral(): boolean;
	static literal(path: string): Pattern;
	matches(path: string): boolean;
	overlaps(other: Pattern): boolean;
	static parse(text: string): Pattern;
	rebase(root: string): Patterns;
	rooted(root: string): Pattern;
	readonly segments: readonly Segment[];
	specificity(): Specificity;
	static subtree(path: string): Pattern;
	readonly text: string;
	toJSON(): string;
	toString(): string;
}

export class Patterns implements Iterable<Pattern> {
	constructor(patterns?: Iterable<Pattern>);
	[iterator](): Iterator<Pattern>;
	contains(pattern: Pattern): boolean;
	covers(other: Patterns): boolean;
	equals(other: Patterns): boolean;
	insert(pattern: Pattern): boolean;
	intersect(other: Patterns): Patterns;
	matches(path: string): boolean;
	overlaps(pattern: Pattern): boolean;
	rebase(root: string): Patterns;
	rooted(root: string): Patterns;
	get size(): number;
	toArray(): Pattern[];
	toJSON(): string[];
}

export type Segment = {
	readonly kind: "literal";
	readonly value: string;
} | {
	readonly kind: "wildcard";
} | {
	readonly kind: "partial";
	readonly prefix: string;
	readonly suffix: string;
} | {
	readonly kind: "globstar";
};

export interface Specificity {
	readonly exact: boolean;
	readonly head: number;
	readonly literals: number;
	readonly partials: number;
	readonly pinned: number;
	readonly wildcards: number;
}

export function compareSpecificity(a: Specificity, b: Specificity): number;
```
