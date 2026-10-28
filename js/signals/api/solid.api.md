# `@moq/signals/solid`

```ts
import { Accessor } from "solid-js";
import { Getter } from "@moq/signals";
import { Setter } from "solid-js";

export function createAccessor<T>(signal: Getter<T>): Accessor<T>;

export function createPair<T>(signal: import("@moq/signals").Signal<T>): import("solid-js").Signal<T>;

export function createSetter<T>(signal: import("@moq/signals").Signal<T>): Setter<T>;
```
