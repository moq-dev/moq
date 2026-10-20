//! A stream: the video of one worker broadcast, fed by one or more cameras.
//!
//! Each stream owns a thread, because encoders are thread-confined and the
//! producer's RHI thread must never wait on a conversion or an encode. The
//! stream picks which camera renders: the smallest one covering the largest
//! demanded rendition. Every demanded rendition is scaled on the GPU from that
//! one capture, so SD and HD demand together still render once.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use moq_mux::codec::annexb;
use moq_video::encode::{Encoder, Kind};
use moq_video::frame::vulkan::{self, Device};
use moq_video::{Color, Frame, I420, Rate, Size, Surface};
use parking_lot::Mutex;

use crate::protocol::{self, Event, Rendition};
use crate::server::{Output, Sent};
use crate::source::Job;

/// Identifies one worker `Open`: a stream id is only meaningful on the
/// connection that chose it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionId {
    pub connection: u64,
    pub id: u32,
}

/// Every rendition is written and labelled BT.709 limited. `Color::infer` would
/// pick BT.601 for the 240p rendition, but its pixels are scaled from the same
/// conversion, so the label follows the conversion rather than the size.
const COLOR: Color = Color::BT709;

#[derive(Clone, Debug)]
pub(crate) struct Session {
    pub key: SessionId,
    pub renditions: Vec<Rendition>,
}

pub(crate) enum Work {
    Open(Session),
    Close(SessionId),
    Frame(Job),
    /// The first camera attached, so a session waiting for its device can open.
    Attached,
    /// A camera's producer failed: fail this session.
    Fail(SessionId, String),
    Shutdown,
}

pub(crate) struct Stream {
    name: String,
    state: Mutex<State>,
    work: mpsc::Sender<Work>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

struct State {
    session: Option<Session>,
    /// Renditions with subscribers, as a mask of session indices.
    demand: u8,
    /// Renditions whose next frame must be an IDR.
    resync: u8,
    sources: Vec<Attached>,
    /// The Vulkan device every camera renders on, learned from the first. The
    /// encoders import on it, so a stream cannot open before a camera attaches.
    device: Option<Device>,
    /// Why a camera's producer failed, until another camera attaches. Every
    /// session opened meanwhile fails with it rather than wait on video that
    /// may never come.
    failed: Option<String>,
    /// The camera currently rendering.
    target: Option<u64>,
    /// Bumped whenever the target changes, so pictures submitted before a
    /// camera switch are never encoded after it.
    generation: u64,
    /// The simulation time each rendition last encoded. Every camera captures
    /// on the same tick, so a switch can offer a rendition the same instant
    /// twice; its timestamps must strictly increase.
    latest: Vec<Option<u64>>,
}

struct Attached {
    id: u64,
    size: Size,
    wanted: Arc<AtomicBool>,
}

/// What to encode from one picture.
struct Plan {
    session: SessionId,
    renditions: Vec<Planned>,
}

struct Planned {
    index: usize,
    keyframe: bool,
}

impl Stream {
    pub fn spawn(name: String, output: Arc<Output>) -> Result<Arc<Self>> {
        let (work, receiver) = mpsc::channel();
        let stream = Arc::new(Self {
            name,
            state: Mutex::new(State {
                session: None,
                demand: 0,
                resync: 0,
                sources: Vec::new(),
                device: None,
                failed: None,
                target: None,
                generation: 0,
                latest: Vec::new(),
            }),
            work,
            thread: Mutex::new(None),
        });
        let thread = std::thread::Builder::new()
            .name(format!("carla-bridge:{}", stream.name))
            .spawn({
                let stream = stream.clone();
                move || run(&stream, &output, receiver)
            })
            .context("start the stream thread")?;
        *stream.thread.lock() = Some(thread);
        Ok(stream)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn send(&self, work: Work) {
        // Fails only after shutdown, when dropping the work is the right thing:
        // a job hands its slot back as it drops.
        let _ = self.work.send(work);
    }

    pub fn shutdown(&self) {
        self.send(Work::Shutdown);
        if let Some(thread) = self.thread.lock().take() {
            let _ = thread.join();
        }
    }

    pub fn generation(&self) -> u64 {
        self.state.lock().generation
    }

    pub fn attach(
        &self,
        id: u64,
        device: Device,
        size: Size,
        wanted: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut state = self.state.lock();
        match state.device {
            Some(current) if current != device => {
                bail!(
                    "camera renders on {device}, but stream {} encodes on {current}",
                    self.name
                )
            }
            Some(_) => {}
            None => {
                state.device = Some(device);
                self.send(Work::Attached);
            }
        }
        state.failed = None;
        state.sources.push(Attached { id, size, wanted });
        state.retarget();
        Ok(())
    }

    fn device(&self) -> Option<Device> {
        self.state.lock().device
    }

    pub fn detach(&self, id: u64) {
        let mut state = self.state.lock();
        if let Some(attached) = state.sources.iter().find(|source| source.id == id) {
            attached.wanted.store(false, Ordering::Release);
        }
        state.sources.retain(|source| source.id != id);
        state.retarget();
    }

    /// A camera's producer can no longer feed it: stop wanting its pictures and
    /// fail the open session, so the worker errors instead of waiting on video
    /// that will not come.
    pub fn fail_producer(&self, id: u64, message: &str) {
        self.detach(id);
        let message = format!("camera failed: {message}");
        let key = {
            let mut state = self.state.lock();
            state.failed = Some(message.clone());
            state.session.as_ref().map(|session| session.key)
        };
        if let Some(key) = key {
            self.send(Work::Fail(key, message));
        }
    }

    pub fn open(&self, session: Session) {
        let mut state = self.state.lock();
        state.latest = vec![None; session.renditions.len()];
        state.session = Some(session.clone());
        state.demand = 0;
        state.resync = 0;
        state.retarget();
        let failed = state.failed.clone();
        drop(state);
        let key = session.key;
        self.send(Work::Open(session));
        if let Some(message) = failed {
            self.send(Work::Fail(key, message));
        }
    }

    pub fn close(&self, key: SessionId) {
        self.end(key);
        self.send(Work::Close(key));
    }

    /// Stop wanting pictures for a session, keeping its encoders until the
    /// thread sees the matching close.
    fn end(&self, key: SessionId) {
        let mut state = self.state.lock();
        if state
            .session
            .as_ref()
            .is_some_and(|session| session.key == key)
        {
            state.session = None;
            state.demand = 0;
            state.resync = 0;
            state.retarget();
        }
    }

    pub fn demand(&self, key: SessionId, renditions: u8) -> Result<()> {
        let mut state = self.state.lock();
        let count = state.renditions(key)?;
        if u32::from(renditions) >> count != 0 {
            bail!("demand mask {renditions:#b} names renditions beyond {count}");
        }
        // A rendition that gains subscribers starts at an IDR; pictures it
        // skipped while idle are not references anything can use.
        state.resync |= renditions & !state.demand;
        state.demand = renditions;
        state.retarget();
        Ok(())
    }

    pub fn keyframe(&self, key: SessionId, rendition: u8) -> Result<()> {
        let mut state = self.state.lock();
        let count = state.renditions(key)?;
        if usize::from(rendition) >= count {
            bail!("rendition {rendition} is beyond {count}");
        }
        state.resync |= 1 << rendition;
        Ok(())
    }

    fn resync(&self, key: SessionId, rendition: usize) {
        let mut state = self.state.lock();
        if state
            .session
            .as_ref()
            .is_some_and(|session| session.key == key)
        {
            state.resync |= 1 << rendition;
        }
    }

    fn plan(&self, source: u64, generation: u64, timestamp_us: u64) -> Option<Plan> {
        self.state.lock().plan(source, generation, timestamp_us)
    }
}

impl State {
    fn renditions(&self, key: SessionId) -> Result<usize> {
        match &self.session {
            Some(session) if session.key == key => Ok(session.renditions.len()),
            _ => bail!("stream {} is not open", key.id),
        }
    }

    /// What to encode from a picture of `source` at `timestamp_us`, taken at
    /// `generation`, or `None` when nothing should: no session, nobody watching,
    /// or a picture from a camera the stream has since switched away from.
    fn plan(&mut self, source: u64, generation: u64, timestamp_us: u64) -> Option<Plan> {
        if self.generation != generation || self.target != Some(source) {
            return None;
        }
        let session = self.session.as_ref()?;
        let mut renditions = Vec::new();
        for index in 0..session.renditions.len() {
            let bit = 1 << index;
            if self.demand & bit == 0
                || self.latest[index].is_some_and(|latest| latest >= timestamp_us)
            {
                continue;
            }
            self.latest[index] = Some(timestamp_us);
            renditions.push(Planned {
                index,
                keyframe: self.resync & bit != 0,
            });
            // Only once planned: a skipped instant leaves the IDR to the next one.
            self.resync &= !bit;
        }
        Some(Plan {
            session: session.key,
            renditions,
        })
    }

    fn retarget(&mut self) {
        let largest = self.session.as_ref().and_then(|session| {
            session
                .renditions
                .iter()
                .enumerate()
                .filter(|(index, _)| self.demand & (1 << index) != 0)
                .map(|(_, rendition)| Size::new(rendition.width, rendition.height))
                .max_by_key(Size::pixels)
        });
        let target = largest.and_then(|largest| {
            self.sources
                .iter()
                .filter(|source| {
                    source.size.width >= largest.width && source.size.height >= largest.height
                })
                .min_by_key(|source| source.size.pixels())
                .map(|source| source.id)
        });
        if target != self.target {
            self.target = target;
            self.generation += 1;
            // Every rendition restarts from the new camera at an IDR.
            self.resync |= self.demand;
        }
        for source in &self.sources {
            source
                .wanted
                .store(Some(source.id) == target, Ordering::Release);
        }
    }
}

fn run(stream: &Arc<Stream>, output: &Output, work: mpsc::Receiver<Work>) {
    let mut encoding: Option<Encoding> = None;
    // A session opened before any camera attached, so before its device is known.
    let mut waiting: Option<Session> = None;
    while let Ok(work) = work.recv() {
        match work {
            Work::Open(session) => {
                encoding = None;
                waiting = None;
                match stream.device() {
                    Some(device) => encoding = open(stream, output, session, device),
                    None => waiting = Some(session),
                }
            }
            Work::Attached => {
                if let (Some(session), Some(device)) = (waiting.take(), stream.device()) {
                    encoding = open(stream, output, session, device);
                }
            }
            Work::Close(key) => {
                if encoding
                    .as_ref()
                    .is_some_and(|open| open.session.key == key)
                {
                    encoding = None;
                }
                if waiting.as_ref().is_some_and(|session| session.key == key) {
                    waiting = None;
                }
            }
            Work::Frame(job) => {
                if let Err((key, err)) = frame(stream, output, encoding.as_mut(), job) {
                    encoding = None;
                    fail(stream, output, key, &err);
                }
            }
            Work::Fail(key, message) => {
                if encoding
                    .as_ref()
                    .is_some_and(|open| open.session.key == key)
                {
                    encoding = None;
                }
                if waiting.as_ref().is_some_and(|session| session.key == key) {
                    waiting = None;
                }
                fail(stream, output, key, &anyhow::anyhow!(message));
            }
            Work::Shutdown => break,
        }
    }
}

/// Open the encoders for `session` on `device` and send the worker each
/// rendition's parameter sets.
fn open(stream: &Stream, output: &Output, session: Session, device: Device) -> Option<Encoding> {
    let key = session.key;
    match Encoding::open(session, device) {
        Ok((opened, configs)) => {
            for (rendition, init) in configs.into_iter().enumerate() {
                output.send(
                    key.connection,
                    &Event::Config {
                        id: key.id,
                        rendition: rendition as u8,
                        init,
                    },
                );
            }
            Some(opened)
        }
        Err(err) => {
            fail(stream, output, key, &err);
            None
        }
    }
}

/// Report a stream failure to the worker, which treats it as fatal.
fn fail(stream: &Stream, output: &Output, key: SessionId, err: &anyhow::Error) {
    tracing::error!(
        stream = stream.name(),
        err = format!("{err:#}"),
        "stream failed"
    );
    stream.end(key);
    output.send(
        key.connection,
        &Event::Error {
            id: key.id,
            message: format!("{}: {err:#}", stream.name()),
        },
    );
}

fn frame(
    stream: &Stream,
    output: &Output,
    encoding: Option<&mut Encoding>,
    job: Job,
) -> Result<(), (SessionId, anyhow::Error)> {
    let (generation, number, timestamp_us) = (job.generation, job.frame, job.timestamp_us);
    // Publish only a picture an encoder will read: an unread frame never signals
    // completion, which loses its slot. Dropping the job instead hands the slot
    // straight back, which is safe because the producer orders every copy into
    // an image after its previous one.
    let Some(plan) = stream.plan(job.source().id, generation, timestamp_us) else {
        return Ok(());
    };
    let key = plan.session;
    let Some(encoding) = encoding.filter(|open| open.session.key == key) else {
        return Ok(());
    };
    if plan.renditions.is_empty() {
        return Ok(());
    }
    let image = job.publish().map_err(|err| (key, err))?;
    encoding
        .encode(stream, output, image, plan, number, timestamp_us)
        .map_err(|err| (key, err))
}

struct Encoding {
    session: Session,
    encoders: Vec<Encoder>,
    gate: Gate,
}

/// Keeps each rendition's reference chain whole across packets the worker
/// had no room for. A rendition that loses a packet drops everything after it
/// until the IDR requested at the loss is queued, so the worker never receives
/// a frame whose reference is missing.
#[derive(Default)]
struct Gate {
    broken: u8,
}

impl Gate {
    fn admit(&self, rendition: usize, keyframe: bool) -> bool {
        keyframe || self.broken & (1 << rendition) == 0
    }

    /// Record what became of an admitted packet. True when the rendition just
    /// broke and needs an IDR.
    fn sent(&mut self, rendition: usize, keyframe: bool, sent: &Sent) -> bool {
        let bit = 1 << rendition;
        match sent {
            Sent::Queued => {
                if keyframe {
                    self.broken &= !bit;
                }
                false
            }
            Sent::Full => {
                self.broken |= bit;
                true
            }
            Sent::Gone => false,
        }
    }
}

impl Encoding {
    /// Open one encoder per rendition on the cameras' device, and read each
    /// one's parameter sets so the worker can advertise the track before it has
    /// a subscriber.
    fn open(session: Session, device: Device) -> Result<(Self, Vec<Bytes>)> {
        let mut encoders = Vec::with_capacity(session.renditions.len());
        let mut configs = Vec::with_capacity(session.renditions.len());
        for rendition in &session.renditions {
            let config = config(rendition, device)?;
            configs.push(
                probe(&config).with_context(|| format!("probe rendition {}", rendition.name))?,
            );
            encoders.push(
                Encoder::new(&config)
                    .with_context(|| format!("open the encoder for {}", rendition.name))?,
            );
        }
        Ok((
            Self {
                session,
                encoders,
                gate: Gate::default(),
            },
            configs,
        ))
    }

    fn encode(
        &mut self,
        stream: &Stream,
        output: &Output,
        image: vulkan::Frame,
        plan: Plan,
        number: u64,
        timestamp_us: u64,
    ) -> Result<()> {
        let key = plan.session;
        let timestamp =
            moq_net::Timestamp::from_micros(timestamp_us).context("capture timestamp")?;
        // The encoders share one conversion of the capture and each scales it to
        // its rendition on the GPU. The slot's completion is signalled once the
        // last of them has read it, after this frame and its clones drop.
        let frame = Frame::new(Surface::Vulkan(image), timestamp);
        for planned in plan.renditions {
            let encoder = &mut self.encoders[planned.index];
            if planned.keyframe {
                encoder.cut().context("request an IDR")?;
            }
            let packets = encoder.encode(&frame).context("encode")?;
            for packet in packets {
                let keyframe = packet.keyframe;
                if !self.gate.admit(planned.index, keyframe) {
                    continue;
                }
                let event = Event::Frame(protocol::Frame {
                    id: key.id,
                    rendition: planned.index as u8,
                    frame: number,
                    timestamp_us,
                    keyframe,
                    payload: packet.payload,
                });
                let sent = output.try_send(key.connection, &event);
                if self.gate.sent(planned.index, keyframe, &sent) {
                    tracing::warn!(
                        stream = stream.name(),
                        frame = number,
                        "worker queue full; resuming at an IDR"
                    );
                    stream.resync(key, planned.index);
                }
            }
        }
        Ok(())
    }
}

fn config(rendition: &Rendition, device: Device) -> Result<moq_video::encode::Config> {
    let framerate = Rate::new(rendition.framerate, 1).context("framerate")?;
    let mut config = moq_video::encode::Config::new(rendition.width, rendition.height, framerate);
    config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(rendition.bitrate));
    config.kind = Kind::Named("nvenc".into());
    config.color = Some(COLOR);
    config.input = Some(device);
    Ok(config)
}

/// The parameter sets `config` produces, from a throwaway session on the same
/// device fed one synthetic mid-gray picture. A separate session so the one
/// that encodes camera pictures only ever receives the cameras' images.
fn probe(config: &moq_video::encode::Config) -> Result<Bytes> {
    let size = config.size();
    let luma = size.width as usize * size.height as usize;
    let gray = I420::new(size, vec![0x80; luma + luma / 2])?;
    let mut encoder = Encoder::new(config)?;
    let mut packets = encoder.encode(&Frame::new(
        Surface::I420(gray),
        moq_net::Timestamp::from_micros(0)?,
    ))?;
    packets.extend(encoder.finish()?);
    let mut sets = Vec::new();
    for packet in &packets {
        for nal in nals(&packet.payload)? {
            if matches!(nal.first().map(|header| header & 0x1f), Some(7 | 8)) {
                sets.push(nal);
            }
        }
    }
    if sets.is_empty() {
        bail!("the encoder emitted no SPS/PPS");
    }
    Ok(annexb::build_prefix(&sets))
}

fn nals(payload: &Bytes) -> Result<Vec<Bytes>> {
    let mut buf = payload.clone();
    let mut iter = annexb::NalIterator::new(&mut buf);
    let mut nals = Vec::new();
    for nal in iter.by_ref() {
        nals.push(nal?);
    }
    nals.extend(iter.flush()?);
    Ok(nals)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SD: Size = Size {
        width: 426,
        height: 240,
    };
    const HD: Size = Size {
        width: 1280,
        height: 720,
    };
    const KEY: SessionId = SessionId {
        connection: 1,
        id: 1,
    };

    fn rendition(name: &str, size: Size) -> Rendition {
        Rendition {
            name: name.into(),
            width: size.width,
            height: size.height,
            bitrate: 1_000_000,
            framerate: 30,
        }
    }

    /// A stream with the demo's shape: an sd and an hd camera, and the
    /// worker's sd and hd renditions.
    fn state() -> (State, Arc<AtomicBool>, Arc<AtomicBool>) {
        let mut state = State {
            session: Some(Session {
                key: KEY,
                renditions: vec![rendition("sd", SD), rendition("hd", HD)],
            }),
            demand: 0,
            resync: 0,
            sources: Vec::new(),
            device: None,
            failed: None,
            target: None,
            generation: 0,
            latest: vec![None; 2],
        };
        let (sd, hd) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        state.sources.push(Attached {
            id: 10,
            size: SD,
            wanted: sd.clone(),
        });
        state.sources.push(Attached {
            id: 20,
            size: HD,
            wanted: hd.clone(),
        });
        state.retarget();
        (state, sd, hd)
    }

    fn demand(state: &mut State, mask: u8) {
        state.resync |= mask & !state.demand;
        state.demand = mask;
        state.retarget();
    }

    fn planned(plan: &Plan) -> Vec<(usize, bool)> {
        plan.renditions
            .iter()
            .map(|planned| (planned.index, planned.keyframe))
            .collect()
    }

    #[test]
    fn nothing_renders_without_demand() {
        let (mut state, sd, hd) = state();
        assert_eq!(state.target, None);
        assert!(!sd.load(Ordering::Acquire) && !hd.load(Ordering::Acquire));
        assert!(state.plan(10, state.generation, 1).is_none());
    }

    #[test]
    fn renders_the_smallest_camera_covering_the_largest_demand() {
        let (mut state, sd, hd) = state();
        demand(&mut state, 0b01);
        assert_eq!(state.target, Some(10));
        assert!(sd.load(Ordering::Acquire) && !hd.load(Ordering::Acquire));

        // SD and HD together render once, from the HD camera.
        demand(&mut state, 0b11);
        assert_eq!(state.target, Some(20));
        assert!(!sd.load(Ordering::Acquire) && hd.load(Ordering::Acquire));
        let plan = state.plan(20, state.generation, 1).unwrap();
        assert_eq!(planned(&plan), vec![(0, true), (1, true)]);
    }

    #[test]
    fn resuming_and_switching_force_idrs_once() {
        let (mut state, ..) = state();
        demand(&mut state, 0b01);
        let generation = state.generation;
        assert_eq!(
            planned(&state.plan(10, generation, 1).unwrap()),
            vec![(0, true)]
        );
        assert_eq!(
            planned(&state.plan(10, generation, 2).unwrap()),
            vec![(0, false)]
        );

        // Parking and resuming the same camera still starts at an IDR.
        demand(&mut state, 0);
        demand(&mut state, 0b01);
        assert_eq!(
            planned(&state.plan(10, state.generation, 3).unwrap()),
            vec![(0, true)]
        );

        // An explicit keyframe request.
        state.resync |= 0b01;
        assert_eq!(
            planned(&state.plan(10, state.generation, 4).unwrap()),
            vec![(0, true)]
        );
    }

    #[test]
    fn pictures_from_before_a_switch_are_not_encoded() {
        let (mut state, ..) = state();
        demand(&mut state, 0b01);
        let before = state.generation;
        demand(&mut state, 0b11);
        // The SD camera's queued picture, and even an HD picture stamped before
        // the switch, are stale.
        assert!(state.plan(10, before, 1).is_none());
        assert!(state.plan(20, before, 1).is_none());
        assert!(state.plan(20, state.generation, 1).is_some());
    }

    #[test]
    fn a_rendition_never_encodes_an_instant_twice() {
        let (mut state, ..) = state();
        demand(&mut state, 0b01);
        // The SD camera captured tick 5 and it was encoded.
        assert_eq!(
            planned(&state.plan(10, state.generation, 5).unwrap()),
            vec![(0, true)]
        );
        // HD demand arrives mid-tick; the HD camera's picture of the same tick
        // feeds only HD, and SD's IDR waits for the next tick.
        demand(&mut state, 0b11);
        let generation = state.generation;
        assert_eq!(
            planned(&state.plan(20, generation, 5).unwrap()),
            vec![(1, true)]
        );
        assert_eq!(
            planned(&state.plan(20, generation, 6).unwrap()),
            vec![(0, true), (1, false)]
        );
        // Nor does time run backwards.
        assert_eq!(planned(&state.plan(20, generation, 4).unwrap()), vec![]);
    }

    #[test]
    fn losing_the_camera_falls_back_to_one_that_still_covers() {
        let (mut state, sd, hd) = state();
        demand(&mut state, 0b01);
        assert_eq!(state.target, Some(10));
        let generation = state.generation;
        state.sources.retain(|source| source.id != 10);
        state.retarget();
        // The HD camera covers SD too.
        assert_eq!(state.target, Some(20));
        assert!(hd.load(Ordering::Acquire));
        assert!(state.generation > generation);
        let _ = sd;

        // Nothing covers HD once its camera is gone.
        demand(&mut state, 0b10);
        state.sources.clear();
        state.retarget();
        assert_eq!(state.target, None);
    }

    #[test]
    fn closing_the_session_parks_the_cameras() {
        let (mut state, sd, _) = state();
        demand(&mut state, 0b01);
        assert!(sd.load(Ordering::Acquire));
        state.session = None;
        state.demand = 0;
        state.retarget();
        assert!(!sd.load(Ordering::Acquire));
    }

    #[test]
    fn a_lost_packet_drops_its_rendition_until_the_next_idr() {
        let mut gate = Gate::default();
        assert!(gate.admit(0, false));
        assert!(!gate.sent(0, false, &Sent::Queued));
        // The worker had no room: rendition 0 breaks, rendition 1 does not.
        assert!(gate.sent(0, false, &Sent::Full));
        assert!(!gate.admit(0, false));
        assert!(gate.admit(1, false));
        // A keyframe gets through, but only a queued one heals the chain.
        assert!(gate.admit(0, true));
        assert!(gate.sent(0, true, &Sent::Full));
        assert!(!gate.admit(0, false));
        assert!(!gate.sent(0, true, &Sent::Queued));
        assert!(gate.admit(0, false));
        // A departed worker neither breaks nor heals anything.
        assert!(!gate.sent(1, false, &Sent::Gone));
        assert!(gate.admit(1, false));
    }
}
