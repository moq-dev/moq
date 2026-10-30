//! The bridge on real hardware: a stand-in for the CARLA plugin renders on
//! Vulkan, the bridge encodes with NVENC through moq-video, and a worker
//! client on the socket checks every packet and decodes it.
//!
//! Needs a Linux NVIDIA GPU, so it is ignored by default and run by
//! `just carla bridge-gpu`, which `just carla check` calls on such a host.
#![cfg(target_os = "linux")]

mod standin;

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use carla_bridge::Bridge;
use carla_bridge::protocol::{self, Event, Frame, Rendition, Request};
use moq_video::decode::{Config, Decoder, Kind};
use moq_video::{Output, Size};
use standin::{Camera, Gpu};

const STREAM: &str = "truck1/front";
const SD: Size = Size {
    width: 426,
    height: 240,
};
const HD: Size = Size {
    width: 1280,
    height: 720,
};
/// One simulation step at 30 fps.
const STEP_US: u64 = 33_333;

fn renditions() -> Vec<Rendition> {
    vec![
        Rendition {
            name: "sd".into(),
            width: SD.width,
            height: SD.height,
            bitrate: 800_000,
            framerate: 30,
        },
        Rendition {
            name: "hd".into(),
            width: HD.width,
            height: HD.height,
            bitrate: 4_000_000,
            framerate: 30,
        },
    ]
}

/// The worker's end of the socket.
struct Worker {
    socket: UnixStream,
    /// A decoder per rendition, fed every packet in order, so a broken
    /// reference chain shows up as a failed decode or a wrong color.
    decoders: HashMap<u8, Decoding>,
    /// Pictures decoded and color-checked.
    checked: usize,
}

impl Worker {
    fn connect(path: &Path) -> Self {
        let socket = UnixStream::connect(path).expect("connect to the bridge");
        let mut worker = Self {
            socket,
            decoders: HashMap::new(),
            checked: 0,
        };
        worker.send(Request::Hello {
            version: protocol::VERSION,
        });
        assert_eq!(
            worker.next(),
            Event::Hello {
                version: protocol::VERSION
            }
        );
        worker
    }

    fn send(&mut self, request: Request) {
        self.socket
            .write_all(&request.encode())
            .expect("write to the bridge");
    }

    fn next(&mut self) -> Event {
        let (kind, body) = protocol::read(&mut self.socket)
            .expect("read from the bridge")
            .expect("the bridge closed the connection");
        Event::decode(kind, body).expect("decode an event")
    }

    /// Open the stream and check each rendition's parameter sets describe it.
    fn open(&mut self, id: u32) {
        self.send(Request::Open {
            id,
            name: STREAM.into(),
            renditions: renditions(),
        });
        self.decoders.clear();
        for (index, rendition) in renditions().iter().enumerate() {
            let event = self.next();
            let Event::Config {
                id: got,
                rendition: got_rendition,
                init,
            } = event
            else {
                panic!("expected the config for {}, got {event:?}", rendition.name);
            };
            assert_eq!((got, usize::from(got_rendition)), (id, index));
            let config = moq_mux::codec::h264::config(&init).expect("parse the parameter sets");
            assert_eq!(
                (config.coded_width, config.coded_height),
                (Some(rendition.width), Some(rendition.height))
            );
            eprintln!(
                "config {}: {} bytes, {:?}",
                rendition.name,
                init.len(),
                config.codec
            );
        }
    }

    /// The next frame event, checked and decoded.
    fn frame(&mut self, rendition: u8, number: u64, rgb: [u8; 3]) -> Frame {
        let Event::Frame(frame) = self.next() else {
            panic!("expected a frame");
        };
        assert_eq!(
            (frame.rendition, frame.frame),
            (rendition, number),
            "frame out of order"
        );
        assert_eq!(frame.timestamp_us, number * STEP_US);
        self.decode(&frame, rgb);
        frame
    }

    fn decode(&mut self, frame: &Frame, rgb: [u8; 3]) {
        if frame.keyframe {
            let catalog =
                moq_mux::codec::h264::config(&frame.payload).expect("an IDR carries its SPS");
            let mut config = Config::new();
            config.kind = Kind::Named("nvdec".into());
            config.output = Output::Cpu;
            let decoder = Decoder::new(&catalog, &config).expect("open a decoder");
            self.decoders
                .insert(frame.rendition, (decoder, VecDeque::new()));
        }
        let (decoder, expected) = self
            .decoders
            .get_mut(&frame.rendition)
            .expect("the first packet of a rendition is an IDR");
        expected.push_back((frame.frame, rgb));
        let timestamp = moq_net::Timestamp::from_micros(frame.timestamp_us).unwrap();
        // The decoder may hold a picture back; match pictures to
        // packets in order rather than one for one.
        for picture in decoder
            .decode(&frame.payload, timestamp, frame.keyframe)
            .expect("decode the packet")
        {
            let (number, rgb) = expected.pop_front().expect("more pictures than packets");
            let i420 = picture.surface.to_i420().expect("CPU picture");
            let size = SIZES[usize::from(frame.rendition)];
            assert_eq!((i420.width(), i420.height()), (size.width, size.height));
            let (y, u, v) = (mean(i420.y()), mean(i420.u()), mean(i420.v()));
            let (ey, eu, ev) = bt709_limited(rgb);
            assert!(
                (y - ey).abs() < 3.0 && (u - eu).abs() < 3.0 && (v - ev).abs() < 3.0,
                "frame {number} rendition {}: decoded YUV ({y:.1}, {u:.1}, {v:.1}), expected ({ey:.1}, {eu:.1}, {ev:.1}) for {rgb:?}",
                frame.rendition
            );
            self.checked += 1;
        }
    }
}

const SIZES: [Size; 2] = [SD, HD];

/// A rendition's decoder and the frames (number, color) it has yet to output.
type Decoding = (Decoder, VecDeque<(u64, [u8; 3])>);

fn mean(plane: &[u8]) -> f64 {
    plane.iter().map(|&sample| f64::from(sample)).sum::<f64>() / plane.len() as f64
}

/// The display-referred bytes as BT.709 limited-range Y'CbCr, with no transfer
/// function applied: what the bridge's conversion promises.
fn bt709_limited([r, g, b]: [u8; 3]) -> (f64, f64, f64) {
    let (r, g, b) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    (
        16.0 + 219.0 * y,
        128.0 + 224.0 * (b - y) / 1.8556,
        128.0 + 224.0 * (r - y) / 1.5748,
    )
}

/// A distinct, well-inside-gamut color per frame.
fn color(number: u64) -> [u8; 3] {
    let n = number as u32;
    [
        (40 + (n * 53) % 170) as u8,
        (40 + (n * 97 + 64) % 170) as u8,
        (40 + (n * 29 + 128) % 170) as u8,
    ]
}

/// Spin until another thread publishes some state, named by `_what` for the
/// reader. Unbounded, like every read here: a slow host must not fail a
/// correct run.
fn until(_what: &str, mut done: impl FnMut() -> bool) {
    while !done() {
        std::thread::yield_now();
    }
}

/// Latency from submitting a picture to receiving its packets, for the report.
#[derive(Default)]
struct Latency(Vec<Duration>);

impl Latency {
    fn report(&mut self, phase: &str) {
        if self.0.is_empty() {
            return;
        }
        self.0.sort();
        let pick = |q: f64| self.0[((self.0.len() - 1) as f64 * q) as usize];
        eprintln!(
            "{phase}: {} frames, submit->packets p50 {:?} p90 {:?} max {:?}",
            self.0.len(),
            pick(0.5),
            pick(0.9),
            pick(1.0)
        );
        self.0.clear();
    }
}

/// Render frame `number` on `camera` and receive its packet for every
/// rendition in `renditions`, returning which were keyframes.
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "measures real GPU encode latency on the host clock"
)]
fn step(
    camera: &mut Camera,
    worker: &mut Worker,
    latency: &mut Latency,
    number: u64,
    renditions: &[u8],
) -> Vec<bool> {
    let rgb = color(number);
    until("an idle slot", || {
        camera.render(rgb, number, number * STEP_US)
    });
    let submitted = std::time::Instant::now();
    let keyframes = renditions
        .iter()
        .map(|&rendition| worker.frame(rendition, number, rgb).keyframe)
        .collect();
    latency.0.push(submitted.elapsed());
    keyframes
}

fn socket_path() -> PathBuf {
    std::env::temp_dir().join(format!("carla-bridge-test-{}.sock", std::process::id()))
}

#[test]
#[ignore = "needs a Linux NVIDIA GPU; run by `just carla bridge-gpu`"]
fn gpu_video_reaches_the_worker() {
    let gpu = Gpu::open().expect("an NVIDIA Vulkan device");
    let bridge = Bridge::listen(socket_path()).expect("listen");
    let mut sd = Camera::new(&gpu, &bridge, STREAM, SD, 3);
    let mut hd = Camera::new(&gpu, &bridge, STREAM, HD, 3);
    let mut latency = Latency::default();
    let mut number = 0u64;
    let mut next = || {
        number += 1;
        number
    };

    let mut worker = Worker::connect(bridge.path());
    worker.open(1);
    // No subscriber, no rendering.
    assert!(!sd.wanted() && !hd.wanted());

    eprintln!("-- sd demand renders the sd camera only");
    worker.send(Request::Demand {
        id: 1,
        renditions: 0b01,
    });
    until("the sd camera", || sd.wanted());
    assert!(!hd.wanted());
    for i in 0..45 {
        let keyframes = step(&mut sd, &mut worker, &mut latency, next(), &[0]);
        assert_eq!(
            keyframes,
            [i == 0],
            "sd starts at an IDR and then follows its GOP"
        );
    }
    latency.report("sd from the sd camera");

    eprintln!("-- sd and hd demand render once, from the hd camera");
    worker.send(Request::Demand {
        id: 1,
        renditions: 0b11,
    });
    until("the hd camera", || hd.wanted());
    assert!(!sd.wanted());
    for i in 0..45 {
        let keyframes = step(&mut hd, &mut worker, &mut latency, next(), &[0, 1]);
        assert_eq!(
            keyframes,
            [i == 0, i == 0],
            "a camera switch restarts both renditions at an IDR"
        );
    }
    latency.report("sd+hd from the hd camera");

    eprintln!("-- a keyframe request is honored out of cadence");
    worker.send(Request::Keyframe {
        id: 1,
        rendition: 1,
    });
    let mut forced = false;
    for _ in 0..10 {
        let keyframes = step(&mut hd, &mut worker, &mut latency, next(), &[0, 1]);
        assert!(!keyframes[0], "only the requested rendition cuts");
        if keyframes[1] {
            forced = true;
            break;
        }
    }
    assert!(forced, "no IDR followed the request");

    eprintln!("-- no demand parks both cameras; resuming starts at an IDR");
    worker.send(Request::Demand {
        id: 1,
        renditions: 0,
    });
    until("both cameras to park", || !sd.wanted() && !hd.wanted());
    worker.send(Request::Demand {
        id: 1,
        renditions: 0b10,
    });
    until("the hd camera", || hd.wanted());
    for i in 0..10 {
        let keyframes = step(&mut hd, &mut worker, &mut latency, next(), &[1]);
        assert_eq!(keyframes, [i == 0]);
    }
    latency.report("hd from the hd camera");

    eprintln!("-- a restarted worker replaces the old connection");
    let replacement = Worker::connect(bridge.path());
    assert!(
        protocol::read(&mut worker.socket).expect("read").is_none(),
        "the replaced worker's socket was not closed"
    );
    until("the old session to park its camera", || !hd.wanted());
    let checked = worker.checked;
    drop(worker);
    let mut worker = replacement;
    worker.open(7);
    worker.send(Request::Demand {
        id: 7,
        renditions: 0b01,
    });
    until("the sd camera", || sd.wanted());
    for i in 0..5 {
        let keyframes = step(&mut sd, &mut worker, &mut latency, next(), &[0]);
        assert_eq!(keyframes, [i == 0]);
    }

    eprintln!("-- closing a camera with work in flight waits for the GPU");
    worker.send(Request::Demand {
        id: 7,
        renditions: 0b11,
    });
    until("the hd camera", || hd.wanted());
    let in_flight = next();
    until("an idle slot", || {
        hd.render(color(in_flight), in_flight, in_flight * STEP_US)
    });
    drop(hd);
    // Nothing left covers hd, so nothing renders.
    until("the sd camera to park", || !sd.wanted());
    worker.send(Request::Demand {
        id: 7,
        renditions: 0b01,
    });
    until("the sd camera", || sd.wanted());
    let resumed = next();
    let rgb = color(resumed);
    until("an idle slot", || {
        sd.render(rgb, resumed, resumed * STEP_US)
    });
    // The closed camera's last picture may or may not have been encoded.
    let frame = loop {
        let Event::Frame(frame) = worker.next() else {
            panic!("expected a frame");
        };
        if frame.frame != in_flight {
            break frame;
        }
    };
    assert_eq!(frame.frame, resumed);
    assert!(frame.keyframe);

    eprintln!("-- a camera whose producer fails fails the worker's stream");
    sd.source().fail("the copy could not be submitted");
    let Event::Error { id: 7, message } = worker.next() else {
        panic!("expected the stream to fail");
    };
    assert!(
        message.contains(STREAM) && message.contains("the copy could not be submitted"),
        "{message}"
    );
    until("the failed camera to park", || !sd.wanted());

    eprintln!("-- the failure outlives the session until a camera replaces it");
    let checked = checked + worker.checked;
    drop(worker);
    let mut worker = Worker::connect(bridge.path());
    worker.open(9);
    let Event::Error { id: 9, message } = worker.next() else {
        panic!("expected the next session to fail");
    };
    assert!(
        message.contains("the copy could not be submitted"),
        "{message}"
    );
    // The worker treats a stream error as fatal, and its replacement respawns
    // the cameras.
    drop(worker);
    drop(sd);
    let sd = Camera::new(&gpu, &bridge, STREAM, SD, 3);
    let mut worker = Worker::connect(bridge.path());
    worker.open(10);
    worker.send(Request::Demand {
        id: 10,
        renditions: 0b01,
    });
    until("the replacement sd camera", || sd.wanted());

    eprintln!("-- a departed worker parks the cameras");
    let checked = checked + worker.checked;
    drop(worker);
    until("the sd camera to park", || !sd.wanted());

    eprintln!("-- malformed requests end the connection with an error");
    let mut worker = Worker::connect(bridge.path());
    let mut odd = renditions();
    odd[0].width = 425;
    worker.send(Request::Open {
        id: 1,
        name: STREAM.into(),
        renditions: odd,
    });
    let Event::Error { id: 0, message } = worker.next() else {
        panic!("expected a connection error");
    };
    assert!(message.contains("even"), "{message}");
    assert!(protocol::read(&mut worker.socket).expect("read").is_none());

    eprintln!("dropped for want of a slot: {}", sd.source().dropped());
    assert!(
        checked > 100,
        "only {checked} pictures were decoded and checked"
    );
    drop(sd);
}
