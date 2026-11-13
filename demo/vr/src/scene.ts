import { type Hang, Signals } from "@moq/watch";
import * as THREE from "three";
import { VRButton } from "three/addons/webxr/VRButton.js";
import type { Spatial } from "./audio";

type Projection = Hang.Catalog.Projection;

const EYE = 1.6;
const SCREEN_HEIGHT = 1.8;
const SPHERE_RADIUS = 10;
const HUD_WIDTH = 0.9;
const HUD_LINES = 4;
const HUD_LINE = 56;
// Where the panel sits relative to the head: below the line of sight, at arm's length.
const HUD_OFFSET = new THREE.Vector3(0, -0.55, -1.2);
// How far the head turns from the panel before it swings back in front.
const HUD_FOLLOW = THREE.MathUtils.degToRad(35);
const UP = new THREE.Vector3(0, 1, 0);

// Each eye's [u, v, width, height] in the frame.
const PACKING: Record<string, [THREE.Vector4Tuple, THREE.Vector4Tuple]> = {
	mono: [
		[0, 0, 1, 1],
		[0, 0, 1, 1],
	],
	"left-right": [
		[0, 0, 0.5, 1],
		[0.5, 0, 0.5, 1],
	],
	"top-bottom": [
		[0, 0.5, 1, 0.5],
		[0, 0, 1, 0.5],
	],
};

// Paints decoded frames onto a screen or sphere according to projection
export class Scene {
	// Screen centre in content space, shared as the point-audio source.
	readonly speaker = new THREE.Vector3(0, EYE, -2);

	// Toggled by a controller squeeze or the M key.
	readonly muted = new Signals.Signal(false);

	// Rendered and decoded frames per second, updated once a second.
	readonly fps = new Signals.Signal({ render: 0, video: 0 });

	// Posed in content space each frame, so audio turns with the picture on a recentre.
	listener?: Spatial;

	#renderer: THREE.WebGLRenderer;
	#scene = new THREE.Scene();
	#content = new THREE.Group();
	#camera: THREE.PerspectiveCamera;
	#texture = new THREE.VideoFrameTexture();
	#frame?: VideoFrame;
	#eyes: THREE.Mesh<THREE.BufferGeometry, THREE.MeshBasicMaterial>[];
	#hud: THREE.Mesh<THREE.PlaneGeometry, THREE.MeshBasicMaterial>;
	#hudCanvas = document.createElement("canvas");
	// The panel's current and target yaw; unset snaps it in front on the next frame.
	#hudYaw?: number;
	#hudTarget = 0;
	#headPosition = new THREE.Vector3();
	#headDirection = new THREE.Vector3();
	#relative = new THREE.Matrix4();
	#renders = 0;
	#frames = 0;
	#since = performance.now();

	#projection?: Projection;
	#stereo = "mono";
	#aspect = 16 / 9;

	constructor(parent: HTMLElement) {
		this.#renderer = new THREE.WebGLRenderer({ antialias: true });
		this.#renderer.setPixelRatio(window.devicePixelRatio);
		this.#renderer.setSize(window.innerWidth, window.innerHeight);
		this.#renderer.xr.enabled = true;
		this.#renderer.xr.setReferenceSpaceType("local-floor");
		parent.append(this.#renderer.domElement, VRButton.createButton(this.#renderer));

		this.#camera = new THREE.PerspectiveCamera(70, window.innerWidth / window.innerHeight, 0.01, 100);
		this.#camera.position.set(0, EYE, 0);
		this.#camera.layers.enable(1);

		this.#scene.background = new THREE.Color(0x111111);
		this.#scene.add(this.#content);
		this.#texture.colorSpace = THREE.SRGBColorSpace;

		this.#eyes = [1, 2].map((layer) => {
			const map = this.#texture.clone();
			const mesh = new THREE.Mesh(
				new THREE.BufferGeometry(),
				new THREE.MeshBasicMaterial({ map, toneMapped: false }),
			);
			mesh.layers.set(layer);
			mesh.visible = false;
			this.#content.add(mesh);
			return mesh;
		});
		this.#build();

		this.#hudCanvas.width = 1024;
		this.#hudCanvas.height = HUD_LINES * HUD_LINE + 32;
		const hudMap = new THREE.CanvasTexture(this.#hudCanvas);
		hudMap.colorSpace = THREE.SRGBColorSpace;
		this.#hud = new THREE.Mesh(
			new THREE.PlaneGeometry(HUD_WIDTH, (HUD_WIDTH * this.#hudCanvas.height) / this.#hudCanvas.width),
			new THREE.MeshBasicMaterial({ map: hudMap, transparent: true, toneMapped: false }),
		);
		// Outside the content, since it follows the head rather than the picture.
		this.#scene.add(this.#hud);
		this.#renderer.xr.addEventListener("sessionstart", () => {
			this.#hudYaw = undefined;
		});

		// Trigger or pinch recentres; grip toggles mute.
		for (const index of [0, 1]) {
			const controller = this.#renderer.xr.getController(index);
			controller.addEventListener("select", this.#recentre);
			controller.addEventListener("squeeze", this.#toggleMute);
		}

		window.addEventListener("resize", this.#resize);
		window.addEventListener("keydown", this.#key);
		this.#look(this.#renderer.domElement);

		this.#renderer.setAnimationLoop(() => {
			this.#follow();
			this.#renderer.render(this.#scene, this.#camera);
			this.#relative.copy(this.#content.matrixWorld).invert().multiply(this.#head().matrixWorld);
			this.listener?.pose(this.#relative);
			this.#renders++;
			this.#count();
		});
	}

	frame(frame: VideoFrame) {
		this.#frames++;
		this.#texture.setFrame(frame);
		for (const mesh of this.#eyes) {
			if (mesh.material.map) mesh.material.map.needsUpdate = true;
			mesh.visible = true;
		}
		this.#frame?.close();
		this.#frame = frame;

		const aspect = frame.displayWidth / frame.displayHeight;
		if (aspect !== this.#aspect) {
			this.#aspect = aspect;
			this.#build();
		}
	}

	// Layout for the picture according to projection and stereo
	layout(projection?: Projection, stereo?: string) {
		this.#projection = projection;
		this.#stereo = stereo && stereo in PACKING ? stereo : "mono";
		this.#build();
	}

	// Up to HUD_LINES lines on the in-headset panel.
	status(lines: string[]) {
		const context = this.#hudCanvas.getContext("2d");
		if (!context) return;
		const { width, height } = this.#hudCanvas;
		context.clearRect(0, 0, width, height);
		context.fillStyle = "rgba(0, 0, 0, 0.6)";
		context.beginPath();
		context.roundRect(0, 0, width, height, 24);
		context.fill();
		context.fillStyle = "#fff";
		context.font = "40px monospace";
		context.textBaseline = "middle";
		for (const [index, line] of lines.slice(0, HUD_LINES).entries()) {
			context.fillText(line, 32, 16 + HUD_LINE * (index + 0.5), width - 64);
		}
		if (this.#hud.material.map) this.#hud.material.map.needsUpdate = true;
	}

	// Hold the panel still through a glance, then glide it back in front once the head turns away.
	#follow() {
		const head = this.#head();
		head.getWorldPosition(this.#headPosition);
		head.getWorldDirection(this.#headDirection);
		const yaw = Math.atan2(-this.#headDirection.x, -this.#headDirection.z);
		const wrap = (angle: number) => Math.atan2(Math.sin(angle), Math.cos(angle));

		if (this.#hudYaw === undefined) this.#hudYaw = this.#hudTarget = yaw;
		if (Math.abs(wrap(yaw - this.#hudTarget)) > HUD_FOLLOW) this.#hudTarget = yaw;
		this.#hudYaw += wrap(this.#hudTarget - this.#hudYaw) * 0.1;

		this.#hud.position.copy(HUD_OFFSET).applyAxisAngle(UP, this.#hudYaw).add(this.#headPosition);
		// Turned to face the head, then tilted up toward the eyes.
		this.#hud.rotation.set(-0.4, this.#hudYaw, 0, "YXZ");
	}

	#count() {
		const now = performance.now();
		const elapsed = now - this.#since;
		if (elapsed < 1000) return;
		const rate = (count: number) => Math.round((count * 1000) / elapsed);
		this.fps.set({ render: rate(this.#renders), video: rate(this.#frames) });
		this.#renders = 0;
		this.#frames = 0;
		this.#since = now;
	}

	#head(): THREE.Camera {
		return this.#renderer.xr.isPresenting ? this.#renderer.xr.getCamera() : this.#camera;
	}

	// Turn the content to face wherever the viewer is looking.
	#recentre = () => {
		const head = this.#head();
		head.getWorldPosition(this.#headPosition);
		head.getWorldDirection(this.#headDirection);
		const yaw = Math.atan2(-this.#headDirection.x, -this.#headDirection.z);
		this.#content.rotation.set(0, yaw, 0);
this.#content.position.set(this.#headPosition.x, 0, this.#headPosition.z);
		this.#content.updateMatrixWorld();
	};

	#toggleMute = () => {
		this.muted.set(!this.muted.peek());
	};

	#key = (event: KeyboardEvent) => {
		if (event.key === "r") this.#recentre();
		if (event.key === "m") this.#toggleMute();
	};

	#build() {
		const packing = PACKING[this.#stereo];
		const projection = this.#projection?.kind === "equirectangular" ? this.#projection : undefined;

		for (const [eye, mesh] of this.#eyes.entries()) {
			const [u, v, width, height] = packing[eye];
			mesh.material.map?.offset.set(u, v);
			mesh.material.map?.repeat.set(width, height);

			mesh.geometry.dispose();
			if (projection) {
				const fov = THREE.MathUtils.degToRad(projection.fov ?? 360);
				// Centred on the seam opposite phi = 0, then mirrored and turned so the picture's centre
				// faces -Z and its left half wraps to the viewer's left.
				mesh.geometry = new THREE.SphereGeometry(SPHERE_RADIUS, 64, 32, Math.PI - fov / 2, fov).scale(-1, 1, 1);
				mesh.position.set(0, EYE, 0);
				mesh.rotation.y = -Math.PI / 2;
			} else {
				// The packed frame's aspect, unpacked to one eye's.
				const aspect = (this.#aspect * width) / height;
				mesh.geometry = new THREE.PlaneGeometry(SCREEN_HEIGHT * aspect, SCREEN_HEIGHT);
				mesh.position.copy(this.speaker);
				mesh.rotation.y = 0;
			}
		}
	}

	#resize = () => {
		this.#camera.aspect = window.innerWidth / window.innerHeight;
		this.#camera.updateProjectionMatrix();
		this.#renderer.setSize(window.innerWidth, window.innerHeight);
	};

	// Looking around is dragging on desktop; a headset tracks it.
	#look(element: HTMLElement) {
		const euler = new THREE.Euler(0, 0, 0, "YXZ");
		element.addEventListener("pointermove", (event) => {
			if (event.buttons !== 1) return;
			euler.setFromQuaternion(this.#camera.quaternion);
			euler.y += event.movementX * 0.005;
			euler.x = THREE.MathUtils.clamp(euler.x + event.movementY * 0.005, -Math.PI / 2, Math.PI / 2);
			this.#camera.quaternion.setFromEuler(euler);
		});
	}

	close() {
		this.#renderer.setAnimationLoop(null);
		window.removeEventListener("resize", this.#resize);
		window.removeEventListener("keydown", this.#key);
		for (const index of [0, 1]) {
			const controller = this.#renderer.xr.getController(index);
			controller.removeEventListener("select", this.#recentre);
			controller.removeEventListener("squeeze", this.#toggleMute);
		}
		for (const mesh of [...this.#eyes, this.#hud]) {
			mesh.geometry.dispose();
			mesh.material.map?.dispose();
			mesh.material.dispose();
		}
		this.#texture.dispose();
		this.#renderer.dispose();
		this.#frame?.close();
	}
}
