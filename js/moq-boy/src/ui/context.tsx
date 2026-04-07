import { Signals } from "@moq/lite";
import { createAccessor } from "@moq/signals/solid";
import type { JSX } from "solid-js";
import { createContext, onCleanup } from "solid-js";
import type MoqBoy from "../element";
import type { Game } from "../index.ts";
import type { GameStatus } from "../schemas.ts";

export type BoyUIContextValues = {
	boy: MoqBoy;
	connectionStatus: () => string;
	games: () => ReadonlyMap<string, Game>;
	expanded: () => string | undefined;
	setExpanded: (id: string | undefined) => void;
};

export const BoyUIContext = createContext<BoyUIContextValues>();

type BoyUIContextProviderProps = {
	boy: MoqBoy;
	children: JSX.Element;
};

export default function BoyUIContextProvider(props: BoyUIContextProviderProps) {
	const signals = new Signals.Effect();

	const connectionStatus = createAccessor(props.boy.connection.status);
	const games = createAccessor(props.boy.games);
	const expanded = createAccessor(props.boy.expanded);

	const setExpanded = (id: string | undefined) => {
		props.boy.expanded.set(id);
	};

	onCleanup(() => signals.close());

	const value: BoyUIContextValues = {
		boy: props.boy,
		connectionStatus,
		games,
		expanded,
		setExpanded,
	};

	return <BoyUIContext.Provider value={value}>{props.children}</BoyUIContext.Provider>;
}

/** Context for a single Game instance within a GameCard. */
export type GameUIContextValues = {
	game: Game;
	active: () => boolean;
	status: () => GameStatus | undefined;
	viewerId: () => string | undefined;
	expanded: () => boolean;
};

export const GameUIContext = createContext<GameUIContextValues>();

type GameUIContextProviderProps = {
	game: Game;
	children: JSX.Element;
};

export function GameUIContextProvider(props: GameUIContextProviderProps) {
	const active = createAccessor(props.game.active);
	const status = createAccessor(props.game.status);
	const viewerId = createAccessor(props.game.viewerId);
	const globalExpanded = createAccessor(props.game.expanded);

	const expanded = () => globalExpanded() === props.game.sessionId;

	const value: GameUIContextValues = {
		game: props.game,
		active,
		status,
		viewerId,
		expanded,
	};

	return <GameUIContext.Provider value={value}>{props.children}</GameUIContext.Provider>;
}
