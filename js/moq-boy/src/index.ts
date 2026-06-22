/**
 * MoQ Boy: a Game Boy streaming viewer built on `@moq/watch`.
 *
 * @module
 */
import * as Moq from "@moq/net";
import * as Watch from "@moq/watch";

export type { default as MoqBoy } from "./element.tsx";
export type { GameConfig } from "./game.ts";
export { Game, KEY_MAP } from "./game.ts";
export type { GameStats, GameStatus } from "./schemas.ts";
export { GameStatsSchema, GameStatusSchema } from "./schemas.ts";
export { Moq, Watch };
