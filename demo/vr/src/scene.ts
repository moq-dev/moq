import type { Hang } from "@moq/watch";
import * as THREE from "three";
import { VRButton } from "three/addons/webxr/VRButton.js";
import type { Spatial } from "./audio";

type Projection = Hang.Catalog.Projection;

const EYE = 1.6;
const SCREEN_HEIGHT = 1.8;
const SPHERE_RADIUS = 10;

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
	readonly speaker = new THREE.Vector3(0, EYE, -2);

	// follow viewer's head per-frame
	listener?: Spatial;

	#renderer: THREE.WebGLRenderer;
	#scene = new THREE.Scene();
	#camera: THREE.PerspectiveCamera;
	#texture = new THREE.VideoFrameTexture();
	#frame?: VideoFrame;
	#eyes: THREE.Mesh<THREE.BufferGeometry, THREE.MeshBasicMaterial>[];

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
		this.#texture.colorSpace = THREE.SRGBColorSpace;

		this.#eyes = [1, 2].map((layer) => {
			const map = this.#texture.clone();
			const mesh = new THREE.Mesh(
				new THREE.BufferGeometry(),
				new THREE.MeshBasicMaterial({ map, toneMapped: false }),
			);
			mesh.layers.set(layer);
			mesh.visible = false;
			this.#scene.add(mesh);
			return mesh;
		});
		this.#build();

		window.addEventListener("resize", this.#resize);
		this.#look(this.#renderer.domElement);

		this.#renderer.setAnimationLoop(() => {
			this.#renderer.render(this.#scene, this.#camera);
			const head = this.#renderer.xr.isPresenting ? this.#renderer.xr.getCamera() : this.#camera;
			this.listener?.pose(head.matrixWorld);
		});
	}

	frame(frame: VideoFrame) {
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

// Looking around is dragging on desktop, on headset its tracked
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
		for (const mesh of this.#eyes) {
			mesh.geometry.dispose();
			mesh.material.map?.dispose();
			mesh.material.dispose();
		}
		this.#texture.dispose();
		this.#renderer.dispose();
		this.#frame?.close();
	}
}
