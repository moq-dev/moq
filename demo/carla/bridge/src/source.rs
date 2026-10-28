//! A capture source: one CARLA camera's exportable image slots.
//!
//! The producer (the CARLA plugin on Unreal's RHI thread) owns the Vulkan
//! images; the bridge holds their exported handles as moq-video slots, which
//! the encoders import on the producer's device. A slot cycles
//!
//! ```text
//! Idle -> Acquired (producer copies the render target and signals `ready`)
//!      -> Queued (waiting for the stream thread)
//!      -> InFlight (the encoders wait `ready`, read, and signal `complete`)
//!      -> Idle once the GPU has finished
//! ```
//!
//! Every producer call is non-blocking: a slot comes back through a completion
//! the producer checks on its next acquire, and a producer with no idle slot
//! drops that frame before anything encodes it. Only [`Source::close`] waits,
//! so the producer can free its images afterwards.

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use moq_video::Size;
use moq_video::frame::vulkan::{
    self, Completion, Device, Format, Handles, Image, Memory, Slot, Timeline,
};
use parking_lot::{Condvar, Mutex};

use crate::stream::{Stream, Work};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One camera feeding a stream.
pub struct Source {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub id: u64,
    pub size: Size,
    format: Format,
    device: Device,
    pub stream: Arc<Stream>,
    /// Set by the stream: whether this camera is the one it wants pictures from.
    pub wanted: Arc<AtomicBool>,
    slots: Mutex<Slots>,
    /// Signalled whenever a queued slot leaves the queue.
    settled: Condvar,
    /// Frames the producer rendered with no idle slot to copy into.
    dropped: AtomicU64,
}

struct Slots {
    states: Vec<State>,
    /// The last timeline value handed out per slot. Values only grow: a
    /// cancelled or unpublished use just skips ahead.
    values: Vec<u64>,
    closed: bool,
}

enum State {
    /// Not imported yet.
    Empty,
    Idle(Slot<()>),
    /// With the stream generation at acquire time, so a switch away and back
    /// before submit cannot relabel this picture as the resumed stream's.
    Acquired(Slot<()>, Timeline, u64),
    Queued,
    InFlight(Completion<()>),
    /// The GPU failed after publishing, and the slot was released rather than
    /// handed back for reuse.
    Failed,
}

impl Source {
    pub(crate) fn new(
        stream: Arc<Stream>,
        device: Device,
        size: Size,
        format: Format,
        slots: usize,
    ) -> Result<Self> {
        if slots == 0 {
            bail!("a source needs at least one slot");
        }
        if size.width == 0
            || size.height == 0
            || !size.width.is_multiple_of(2)
            || !size.height.is_multiple_of(2)
        {
            bail!("capture size {size} must be non-zero and even");
        }
        let inner = Arc::new(Inner {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            size,
            format,
            device,
            stream,
            wanted: Arc::new(AtomicBool::new(false)),
            slots: Mutex::new(Slots {
                states: (0..slots).map(|_| State::Empty).collect(),
                values: vec![0; slots],
                closed: false,
            }),
            settled: Condvar::new(),
            dropped: AtomicU64::new(0),
        });
        inner
            .stream
            .attach(inner.id, device, size, inner.wanted.clone())?;
        Ok(Self { inner })
    }

    /// Import slot `index`: a dedicated optimal-tiling image of the capture
    /// size and format, allocated from the device's `memory_type`, in
    /// `VK_IMAGE_LAYOUT_GENERAL` whenever the bridge owns it, and a timeline
    /// semaphore starting at 0. The descriptors are consumed whatever happens.
    pub fn import(
        &self,
        index: usize,
        memory: OwnedFd,
        timeline: OwnedFd,
        allocation_size: u64,
        memory_type: u32,
    ) -> Result<()> {
        let mut slots = self.inner.slots.lock();
        if slots.closed {
            bail!("source is closed");
        }
        match slots.states.get(index) {
            Some(State::Empty) => {}
            Some(_) => bail!("slot {index} is already imported"),
            None => bail!(
                "slot {index} is out of range for {} slots",
                slots.states.len()
            ),
        }
        let image = Image {
            device: self.inner.device,
            memory: Memory::OpaqueFd { memory_type },
            size: self.inner.size,
            allocation_size,
            format: self.inner.format,
        };
        let slot = Slot::new(Handles { memory, timeline }, image, ())
            .with_context(|| format!("import slot {index}"))?;
        slots.states[index] = State::Idle(slot);
        Ok(())
    }

    /// Whether the stream wants this camera's pictures now. Render and submit
    /// only while this holds; it is a single atomic load.
    pub fn wanted(&self) -> bool {
        self.inner.wanted.load(Ordering::Acquire)
    }

    /// Take an idle slot for the next picture: its index and the timeline value
    /// the producer's queue submission must signal once the copy has landed.
    ///
    /// `None` means every slot is still in use, and the producer drops this
    /// picture: nothing has been encoded from it, so no reference chain breaks.
    /// The producer must order each copy after the previous use of the same
    /// image on its queue, as any reuse of a render target already is.
    pub fn acquire(&self) -> Result<Option<(usize, u64)>> {
        let mut guard = self.inner.slots.lock();
        let slots = &mut *guard;
        if slots.closed {
            bail!("source is closed");
        }
        reap(&mut slots.states);
        if slots
            .states
            .iter()
            .all(|state| matches!(state, State::Failed))
        {
            bail!("every slot failed on the GPU");
        }
        let Some(index) = slots
            .states
            .iter()
            .position(|state| matches!(state, State::Idle(_)))
        else {
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        };
        let State::Idle(slot) = std::mem::replace(&mut slots.states[index], State::Empty) else {
            unreachable!()
        };
        let ready = slots.values[index] + 1;
        let timeline = Timeline::new(ready, ready + 1).expect("complete follows ready");
        slots.values[index] = ready + 1;
        let generation = self.inner.stream.generation();
        slots.states[index] = State::Acquired(slot, timeline, generation);
        Ok(Some((index, ready)))
    }

    /// Hand an acquired slot to the encoder once the producer's queue
    /// submission, including its `ready` signal, has been made.
    ///
    /// `frame` is CARLA's frame counter and `timestamp_us` the simulation time
    /// the picture shows; both ride through to the worker with the packet.
    pub fn submit(&self, index: usize, frame: u64, timestamp_us: u64) -> Result<()> {
        let job = {
            let mut slots = self.inner.slots.lock();
            let Some(State::Acquired(..)) = slots.states.get(index) else {
                bail!("slot {index} was not acquired");
            };
            let State::Acquired(slot, timeline, generation) =
                std::mem::replace(&mut slots.states[index], State::Queued)
            else {
                unreachable!()
            };
            Job {
                source: self.inner.clone(),
                index,
                slot: Some(slot),
                timeline,
                generation,
                frame,
                timestamp_us,
            }
        };
        // A stream thread that has shut down drops the job, which hands the
        // slot straight back.
        self.inner.stream.send(Work::Frame(job));
        Ok(())
    }

    /// Return an acquired slot the producer did not fill.
    pub fn cancel(&self, index: usize) -> Result<()> {
        let mut slots = self.inner.slots.lock();
        let Some(State::Acquired(..)) = slots.states.get(index) else {
            bail!("slot {index} was not acquired");
        };
        let State::Acquired(slot, ..) = std::mem::replace(&mut slots.states[index], State::Empty)
        else {
            unreachable!()
        };
        slots.states[index] = State::Idle(slot);
        Ok(())
    }

    /// The producer can no longer feed this camera, for `message`. The stream
    /// stops wanting it and fails the worker's session; close it next.
    pub fn fail(&self, message: &str) {
        self.inner.stream.fail_producer(self.inner.id, message);
    }

    /// Frames dropped so far because every slot was busy.
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Stop capturing and release every import once the GPU is done with it. Returns only after that, so the producer may destroy its images and
    /// semaphores next. The stream keeps encoding from any other camera.
    pub fn close(self) {
        drop(self);
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.inner.stream.detach(self.inner.id);
        let mut slots = self.inner.slots.lock();
        slots.closed = true;
        // A queued slot is about to be published or handed back; wait for it
        // rather than guess, since publishing starts GPU work on the image.
        while slots
            .states
            .iter()
            .any(|state| matches!(state, State::Queued))
        {
            self.inner.settled.wait(&mut slots);
        }
        let states = std::mem::take(&mut slots.states);
        drop(slots);
        for state in states {
            if let State::InFlight(completion) = state {
                // Resolves once the encoders have signalled completion. On a device
                // error the slot is released instead; either way nothing on the GPU
                // still references the image afterwards.
                let _ = pollster::block_on(completion.wait());
            }
        }
    }
}

/// Move every slot the GPU has finished with back to idle, without waiting.
fn reap(states: &mut [State]) {
    for state in states.iter_mut() {
        let State::InFlight(completion) = state else {
            continue;
        };
        match completion.try_wait() {
            Ok(Some(slot)) => *state = State::Idle(slot),
            Ok(None) => {}
            Err(err) => {
                tracing::error!(%err, "capture slot lost");
                *state = State::Failed;
            }
        }
    }
}

/// One submitted picture on its way to the stream thread.
pub(crate) struct Job {
    source: Arc<Inner>,
    index: usize,
    slot: Option<Slot<()>>,
    timeline: Timeline,
    /// The stream generation at submission: a picture from before a camera
    /// switch is never encoded into the new stream.
    pub generation: u64,
    pub frame: u64,
    pub timestamp_us: u64,
}

impl Job {
    pub fn source(&self) -> &Arc<Inner> {
        &self.source
    }

    /// Publish the producer's copy. The slot stays in flight until the returned
    /// frame, and every encoder's read of it, has finished on the GPU.
    pub fn publish(mut self) -> Result<vulkan::Frame> {
        let slot = self.slot.take().expect("job published twice");
        let result = slot.publish(self.timeline);
        let mut slots = self.source.slots.lock();
        let published = match result {
            Ok((frame, completion)) => {
                slots.states[self.index] = State::InFlight(completion);
                Ok(frame)
            }
            Err(err) => {
                let (error, slot) = err.into_parts();
                slots.states[self.index] = State::Idle(slot);
                Err(anyhow::Error::from(error).context("publish the capture slot"))
            }
        };
        drop(slots);
        self.source.settled.notify_all();
        published
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Never published, so the GPU holds nothing of the image.
        if let Some(slot) = self.slot.take() {
            let mut slots = self.source.slots.lock();
            slots.states[self.index] = State::Idle(slot);
            drop(slots);
            self.source.settled.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;
    use crate::server::Output;

    fn device(byte: u8) -> Device {
        Device {
            device_uuid: [byte; 16],
            driver_uuid: [0; 16],
            render_node: None,
        }
    }

    fn handle() -> OwnedFd {
        File::open("/dev/null").unwrap().into()
    }

    #[test]
    fn a_picture_nobody_encodes_returns_its_slot() {
        let stream = Stream::spawn("truck1/front".into(), Arc::new(Output::default())).unwrap();
        let size = Size::new(64, 64);
        let source = Source::new(stream.clone(), device(1), size, Format::Bgra8, 1).unwrap();
        source
            .import(0, handle(), handle(), 64 * 64 * 4, 0)
            .unwrap();

        let (index, _) = source.acquire().unwrap().expect("an idle slot");
        source.submit(index, 1, 1).unwrap();
        // No session wants the picture. Publishing it would leave a frame no
        // encoder reads, which never completes and loses the only slot.
        stream.shutdown();
        assert!(source.acquire().unwrap().is_some());
    }

    #[test]
    fn every_camera_of_a_stream_renders_on_one_device() {
        let stream = Stream::spawn("truck1/front".into(), Arc::new(Output::default())).unwrap();
        let size = Size::new(64, 64);
        let _first = Source::new(stream.clone(), device(1), size, Format::Bgra8, 1).unwrap();
        let _same = Source::new(stream.clone(), device(1), size, Format::Rgba8, 1).unwrap();
        let err = Source::new(stream.clone(), device(2), size, Format::Bgra8, 1)
            .err()
            .expect("a camera on another device");
        assert!(err.to_string().contains("encodes on"), "{err:#}");
        stream.shutdown();
    }
}
