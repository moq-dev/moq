# `@moq/signals/react`

```ts
import { Getter } from "@moq/signals";
import { Signal } from "@moq/signals";

export function useSignal<T>(signal: Signal<T>): [T, (value: T | ((prev: T) => T)) => void];

export function useValue<T>(signal: Getter<T>): T;
```
