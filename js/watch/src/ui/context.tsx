import { type Moq, Signals } from "@moq/hang";
import { createAccessor } from "@moq/signals/solid";
import type { JSX } from "solid-js";
import { createContext, createSignal, onCleanup } from "solid-js";
import type { BufferedRanges } from "..";
import type MoqWatch from "../element";

type WatchUIContextProviderProps = {
	moqWatch: MoqWatch;
	children: JSX.Element;
};

export type WatchStatus = "no-url" | "disconnected" | "connecting" | "offline" | "loading" | "live" | "connected";

export type Rendition = {
	name: string;
	width?: number;
	height?: number;
};

export type WatchUIContextValues = {
	moqWatch: MoqWatch;
	watchStatus: () => WatchStatus;
	isPlaying: () => boolean;
	isMuted: () => boolean;
	setVolume: (vol: number) => void;
	currentVolume: () => number;
	togglePlayback: () => void;
	toggleMuted: () => void;
	buffering: () => boolean;
	jitter: () => Moq.Time.Milli;
	setJitter: (value: Moq.Time.Milli) => void;
	availableRenditions: () => Rendition[];
	activeRendition: () => string | undefined;
	setActiveRendition: (name: string | undefined) => void;
	isStatsPanelVisible: () => boolean;
	setIsStatsPanelVisible: (visible: boolean) => void;
	isFullscreen: () => boolean;
	toggleFullscreen: () => void;
	timestamp: () => Moq.Time.Milli | undefined;
	videoBuffered: () => BufferedRanges;
	audioBuffered: () => BufferedRanges;
};

export const WatchUIContext = createContext<WatchUIContextValues>();

export default function WatchUIContextProvider(props: WatchUIContextProviderProps) {
	const [watchStatus, setWatchStatus] = createSignal<WatchStatus>("no-url");
	const [isPlaying, setIsPlaying] = createSignal<boolean>(false);
	const isMuted = createAccessor(props.moqWatch.backend.audio.muted);
	const [currentVolume, setCurrentVolume] = createSignal<number>(0);
	const buffering = createAccessor(props.moqWatch.backend.video.stalled);
	const jitter = createAccessor(props.moqWatch.backend.jitter);
	const [availableRenditions, setAvailableRenditions] = createSignal<Rendition[]>([]);
	const activeRendition = createAccessor(props.moqWatch.backend.video.source.track);
	const [isStatsPanelVisible, setIsStatsPanelVisible] = createSignal<boolean>(false);
	const [isFullscreen, setIsFullscreen] = createSignal<boolean>(!!document.fullscreenElement);

	const togglePlayback = () => {
		props.moqWatch.paused = !props.moqWatch.paused;
	};

	const toggleFullscreen = () => {
		if (document.fullscreenElement) {
			document.exitFullscreen();
		} else {
			props.moqWatch.requestFullscreen();
		}
	};

	const setVolume = (volume: number) => {
		props.moqWatch.backend.audio.volume.set(volume / 100);
	};

	const toggleMuted = () => {
		props.moqWatch.backend.audio.muted.update((muted) => !muted);
	};

	const setJitter = (latency: Moq.Time.Milli) => {
		props.moqWatch.jitter = latency;
	};

	const setActiveRenditionValue = (name: string | undefined) => {
		props.moqWatch.backend.video.source.target.update((prev) => ({
			...prev,
			name: name,
		}));
	};

	const timestamp = createAccessor(props.moqWatch.backend.video.timestamp);
	const videoBuffered = createAccessor(props.moqWatch.backend.video.buffered);
	const audioBuffered = createAccessor(props.moqWatch.backend.audio.buffered);

	const value: WatchUIContextValues = {
		moqWatch: props.moqWatch,
		watchStatus,
		togglePlayback,
		isPlaying,
		setVolume,
		isMuted,
		currentVolume,
		toggleMuted,
		buffering,
		jitter,
		setJitter,
		availableRenditions,
		activeRendition,
		setActiveRendition: setActiveRenditionValue,
		isStatsPanelVisible,
		setIsStatsPanelVisible,
		isFullscreen,
		toggleFullscreen,
		timestamp,
		videoBuffered,
		audioBuffered,
	};

	const watch = props.moqWatch;
	const signals = new Signals.Effect();

	signals.run((effect) => {
		const url = effect.get(watch.connection.url);
		const connection = effect.get(watch.connection.status);
		const broadcast = effect.get(watch.broadcast.status);

		if (!url) {
			setWatchStatus("no-url");
		} else if (connection === "disconnected") {
			setWatchStatus("disconnected");
		} else if (connection === "connecting") {
			setWatchStatus("connecting");
		} else if (broadcast === "offline") {
			setWatchStatus("offline");
		} else if (broadcast === "loading") {
			setWatchStatus("loading");
		} else if (broadcast === "live") {
			setWatchStatus("live");
		} else if (connection === "connected") {
			setWatchStatus("connected");
		}
	});

	signals.run((effect) => {
		const paused = effect.get(watch.backend.paused);
		setIsPlaying(!paused);
	});

	signals.run((effect) => {
		const volume = effect.get(watch.backend.audio.volume);
		setCurrentVolume(volume * 100);
	});

	signals.run((effect) => {
		const videoCatalog = effect.get(watch.backend.video.source.catalog);
		const renditions = videoCatalog?.renditions ?? {};

		const renditionsList: Rendition[] = Object.entries(renditions).map(([name, config]) => ({
			name,
			width: config.codedWidth,
			height: config.codedHeight,
		}));

		setAvailableRenditions(renditionsList);
	});

	const handleFullscreenChange = () => {
		setIsFullscreen(!!document.fullscreenElement);
	};

	signals.event(document, "fullscreenchange", handleFullscreenChange);
	onCleanup(() => signals.close());

	return <WatchUIContext.Provider value={value}>{props.children}</WatchUIContext.Provider>;
}
