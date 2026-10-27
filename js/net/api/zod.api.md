# `@moq/net/zod`

```ts
import { Group } from "@moq/net";
import { Track } from "@moq/net";
import { ZodMiniType } from "@zod/mini";

export function read<T = unknown>(source: Group.Consumer | Track.Ordered, schema: ZodMiniType<T>): Promise<T | undefined>;
```
