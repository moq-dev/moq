import * as THREE from "three";

export type Placement =
	| { kind: "ambisonic" }
	| { kind: "point"; position: THREE.Vector3 }
	| { kind: "head" };

// Virtual speakers at the corners of a cube in ambisonic axes (W, Y, Z, X)
const SPEAKERS = [-1, 1].flatMap((x) =>
	[-1, 1].flatMap((y) => [-1, 1].map((z) => [x, y, z].map((v) => v / Math.sqrt(3)))),
);

const MAX_RE = 1 / Math.sqrt(3);

// Renders audio graph around a viewer's head
export class Spatial {
	#context: BaseAudioContext;
	#nodes: AudioNode[] = [];
	#tracksPosition: boolean;

	constructor(root: AudioNode, placement: Placement) {
		this.#context = root.context;
		const destination = this.#context.destination;

		switch (placement.kind) {
			case "head":
				root.connect(destination);
				this.#tracksPosition = false;
				break;
			case "point": {
				const panner = this.#panner(placement.position, 1);
				root.connect(panner).connect(destination);
				this.#tracksPosition = true;
				break;
			}
			case "ambisonic":
				this.#ambisonic(root, destination);
				// The field is infinitely far, so only the head's rotation moves it.
				this.#tracksPosition = false;
				break;
		}
		this.#nodes.push(root);
	}

	// Decodes out to fixed speakers around the scene
	#ambisonic(root: AudioNode, destination: AudioNode) {
		if (root.channelCount !== 4)
			throw new Error(`first-order ambisonics needs 4 channels, got ${root.channelCount}`);

		const split = new ChannelSplitterNode(this.#context, { numberOfOutputs: 4 });
		root.connect(split);
		this.#nodes.push(split);

		for (const [x, y, z] of SPEAKERS) {
			// ACN order is W, Y, Z, X.
			const weights = [1, 3 * MAX_RE * y, 3 * MAX_RE * z, 3 * MAX_RE * x].map((w) => w / SPEAKERS.length);
			// Ambisonic axes to WebAudio
			const panner = this.#panner(new THREE.Vector3(-y, z, -x), 0);
			panner.connect(destination);

			weights.forEach((weight, channel) => {
				const gain = new GainNode(this.#context, { gain: weight });
				split.connect(gain, channel).connect(panner);
				this.#nodes.push(gain);
			});
		}
	}

	#panner(position: THREE.Vector3, rolloffFactor: number): PannerNode {
		const panner = new PannerNode(this.#context, {
			panningModel: "HRTF",
			distanceModel: "inverse",
			refDistance: 1,
			rolloffFactor,
			positionX: position.x,
			positionY: position.y,
			positionZ: position.z,
		});
		this.#nodes.push(panner);
		return panner;
	}


		// Move the audio with the transform of viewer's head
		pose(head: THREE.Matrix4) {
		const position = new THREE.Vector3();
		const rotation = new THREE.Quaternion();
		head.decompose(position, rotation, new THREE.Vector3());
		const forward = new THREE.Vector3(0, 0, -1).applyQuaternion(rotation);
		const up = new THREE.Vector3(0, 1, 0).applyQuaternion(rotation);

		const listener = this.#context.listener;
		const at = this.#context.currentTime;
		const set = (param: AudioParam, value: number) => param.setTargetAtTime(value, at, 0.01);
		if (!this.#tracksPosition) position.set(0, 0, 0);
		set(listener.positionX, position.x);
		set(listener.positionY, position.y);
		set(listener.positionZ, position.z);
		set(listener.forwardX, forward.x);
		set(listener.forwardY, forward.y);
		set(listener.forwardZ, forward.z);
		set(listener.upX, up.x);
		set(listener.upY, up.y);
		set(listener.upZ, up.z);
	}

	close() {
		for (const node of this.#nodes) node.disconnect();
	}
}
