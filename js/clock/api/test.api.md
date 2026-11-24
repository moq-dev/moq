# `@moq/clock/test`

```ts
import { Duration } from "@moq/clock";
import { Installed } from "@moq/clock";
import { Instant } from "@moq/clock";
import { Wall } from "@moq/clock";

export class Manual {
	constructor();
	static readonly WALL_START: Wall;
	advance(duration: Duration): Promise<void>;
	drive<T>(work: PromiseLike<T>): Promise<T>;
	fire(): Promise<Instant>;
	install(): Installed;
	jump(duration: Duration): void;
	now(): Instant;
	stepWall(wall: Wall): void;
	wall(): Wall;
}

export function hostTime(): void;
```
