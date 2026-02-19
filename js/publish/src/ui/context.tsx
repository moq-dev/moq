import type { JSX } from "solid-js";
import { createContext, createEffect, createSignal } from "solid-js";
import type MoqPublish from "../element";

export type PublishStatus =
	| "no-url"
	| "disconnected"
	| "connecting"
	| "live"
	| "audio-only"
	| "video-only"
	| "select-source";

type PublishUIContextValue = {
	moqPublish: MoqPublish;
	cameraDevices: () => MediaDeviceInfo[];
	microphoneDevices: () => MediaDeviceInfo[];
	publishStatus: () => PublishStatus;
	microphoneActive: () => boolean;
	cameraActive: () => boolean;
	screenActive: () => boolean;
	fileActive: () => boolean;
	nothingActive: () => boolean;
	selectedCameraSource?: () => MediaDeviceInfo["deviceId"] | undefined;
	selectedMicrophoneSource?: () => MediaDeviceInfo["deviceId"] | undefined;
	setFile: (file: File) => void;
};

type PublishUIContextProviderProps = {
	moqPublish: MoqPublish;
	children: JSX.Element;
};

export const PublishUIContext = createContext<PublishUIContextValue>();

export default function PublishUIContextProvider(props: PublishUIContextProviderProps) {
	const [cameraDevices, setCameraMediaDevices] = createSignal<MediaDeviceInfo[]>([]);
	const [selectedCameraSource, setSelectedCameraSource] = createSignal<MediaDeviceInfo["deviceId"] | undefined>();
	const [microphoneDevices, setMicrophoneMediaDevices] = createSignal<MediaDeviceInfo[]>([]);
	const [selectedMicrophoneSource, setSelectedMicrophoneSource] = createSignal<
		MediaDeviceInfo["deviceId"] | undefined
	>();
	const [cameraActive, setCameraActive] = createSignal<boolean>(false);
	const [screenActive, setScreenActive] = createSignal<boolean>(false);
	const [microphoneActive, setMicrophoneActive] = createSignal<boolean>(false);
	const [fileActive, setFileActive] = createSignal<boolean>(false);
	const [nothingActive, setNothingActive] = createSignal<boolean>(false);
	const [publishStatus, setPublishStatus] = createSignal<PublishStatus>("no-url");

	const setFile = (file: File) => {
		props.moqPublish.source.set(file);
		props.moqPublish.invisible.set(false);
		props.moqPublish.muted.set(true);
	};

	const value: PublishUIContextValue = {
		moqPublish: props.moqPublish,
		cameraDevices,
		microphoneDevices,
		publishStatus,
		cameraActive,
		screenActive,
		microphoneActive,
		fileActive,
		setFile,
		nothingActive,
		selectedCameraSource,
		selectedMicrophoneSource,
	};

	createEffect(() => {
		const publish = props.moqPublish;

		// Initialize with "nothing" active on page load
		publish.muted.set(true);
		publish.invisible.set(true);
		publish.source.set(undefined);

		publish.signals.run((effect) => {
			const clearCameraDevices = () => setCameraMediaDevices([]);
			const video = effect.get(publish.video);

			if (!video || !("device" in video)) {
				clearCameraDevices();
				return;
			}

			const devices = effect.get(video.device.available);
			if (!devices || devices.length < 2) {
				clearCameraDevices();
				return;
			}

			setCameraMediaDevices(devices);
		});

		publish.signals.run((effect) => {
			const clearMicrophoneDevices = () => setMicrophoneMediaDevices([]);
			const audio = effect.get(publish.audio);

			if (!audio || !("device" in audio)) {
				clearMicrophoneDevices();
				return;
			}

			const enabled = effect.get(publish.broadcast.audio.enabled);
			if (!enabled) {
				clearMicrophoneDevices();
				return;
			}

			const devices = effect.get(audio.device.available);
			if (!devices || devices.length < 2) {
				clearMicrophoneDevices();
				return;
			}

			setMicrophoneMediaDevices(devices);
		});

		publish.signals.run((effect) => {
			const source = effect.get(publish.source);
			const muted = effect.get(publish.muted);
			const invisible = effect.get(publish.invisible);

			setNothingActive(source === undefined && muted && invisible);
		});

		publish.signals.run((effect) => {
			const audioActive = !effect.get(publish.muted);
			setMicrophoneActive(audioActive);
		});

		publish.signals.run((effect) => {
			const videoSource = effect.get(publish.source);
			const videoActive = effect.get(publish.video);

			if (videoActive && videoSource === "camera") {
				setCameraActive(true);
				setScreenActive(false);
			} else if (videoActive && videoSource === "screen") {
				setScreenActive(true);
				setCameraActive(false);
			} else {
				setCameraActive(false);
				setScreenActive(false);
			}
		});

		publish.signals.run((effect) => {
			const video = effect.get(publish.video);

			if (!video || !("device" in video)) return;

			const requested = effect.get(video.device.requested);
			setSelectedCameraSource(requested);
		});

		publish.signals.run((effect) => {
			const audio = effect.get(publish.audio);

			if (!audio || !("device" in audio)) return;

			const requested = effect.get(audio.device.requested);
			setSelectedMicrophoneSource(requested);
		});

		publish.signals.run((effect) => {
			const url = effect.get(publish.connection.url);
			const status = effect.get(publish.connection.status);
			const audioSource = effect.get(publish.broadcast.audio.source);
			const videoSource = effect.get(publish.broadcast.video.source);
			const muted = effect.get(publish.muted);
			const invisible = effect.get(publish.invisible);

			const audio = audioSource && !muted;
			const video = videoSource && !invisible;

			if (!url) {
				setPublishStatus("no-url");
			} else if (status === "disconnected") {
				setPublishStatus("disconnected");
			} else if (status === "connecting") {
				setPublishStatus("connecting");
			} else if (!audio && !video) {
				setPublishStatus("select-source");
			} else if (!audio && video) {
				setPublishStatus("video-only");
			} else if (audio && !video) {
				setPublishStatus("audio-only");
			} else if (audio && video) {
				setPublishStatus("live");
			}
		});

		publish.signals.run((effect) => {
			const selectedSource = effect.get(publish.source);
			setFileActive(selectedSource instanceof File);
		});
	});

	return <PublishUIContext.Provider value={value}>{props.children}</PublishUIContext.Provider>;
}
