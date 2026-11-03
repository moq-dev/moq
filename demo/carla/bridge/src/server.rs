//! The worker's side of the socket: one connection at a time.
//!
//! The newest connection wins. A restarted worker reconnects while the old
//! socket may still look open, so a new connection shuts the old one down and
//! closes every stream it opened.

use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::mpsc::{self, SyncSender, TrySendError};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use parking_lot::Mutex;

use crate::Shared;
use crate::protocol::{self, Event, MAX_RENDITIONS, Request, VERSION};
use crate::stream::{Session, SessionId, Stream};

/// Encoded messages queued for the worker: about two seconds of two
/// renditions at 30 fps. Beyond that the worker is not keeping up, and a
/// stream drops to its next IDR rather than queue without bound.
const OUTPUT_DEPTH: usize = 128;

/// Where encoded frames go: the current connection's writer, if any.
#[derive(Default)]
pub(crate) struct Output {
    current: Mutex<Option<(u64, SyncSender<Bytes>)>>,
}

pub(crate) enum Sent {
    Queued,
    /// The worker is behind; the message was dropped.
    Full,
    /// That connection is gone; its streams are closing.
    Gone,
}

impl Output {
    fn sender(&self, connection: u64) -> Option<SyncSender<Bytes>> {
        match &*self.current.lock() {
            Some((current, sender)) if *current == connection => Some(sender.clone()),
            _ => None,
        }
    }

    /// Queue a frame without waiting.
    pub fn try_send(&self, connection: u64, event: &Event) -> Sent {
        let Some(sender) = self.sender(connection) else {
            return Sent::Gone;
        };
        match sender.try_send(event.encode()) {
            Ok(()) => Sent::Queued,
            Err(TrySendError::Full(_)) => Sent::Full,
            Err(TrySendError::Disconnected(_)) => Sent::Gone,
        }
    }

    /// Queue a message that must not be dropped, waiting for room.
    pub fn send(&self, connection: u64, event: &Event) {
        if let Some(sender) = self.sender(connection) {
            let _ = sender.send(event.encode());
        }
    }

    fn connect(&self, connection: u64, sender: SyncSender<Bytes>) {
        *self.current.lock() = Some((connection, sender));
    }

    fn disconnect(&self, connection: u64) {
        let mut current = self.current.lock();
        if current
            .as_ref()
            .is_some_and(|(current, _)| *current == connection)
        {
            *current = None;
        }
    }
}

pub(crate) fn listen(shared: &Arc<Shared>, listener: UnixListener) {
    let mut connection = 0u64;
    for socket in listener.incoming() {
        if shared.closing() {
            break;
        }
        let socket = match socket {
            Ok(socket) => socket,
            Err(err) => {
                tracing::error!(%err, "accept a worker");
                continue;
            }
        };
        connection += 1;
        if let Err(err) = accept(shared, connection, socket) {
            tracing::error!(err = format!("{err:#}"), "start a worker connection");
        }
    }
}

fn accept(shared: &Arc<Shared>, connection: u64, socket: UnixStream) -> Result<()> {
    let (sender, receiver) = mpsc::sync_channel::<Bytes>(OUTPUT_DEPTH);
    let reader = socket.try_clone().context("clone the worker socket")?;
    let writer = socket.try_clone().context("clone the worker socket")?;
    if let Some(previous) = shared.replace_connection(connection, socket) {
        tracing::warn!("a new worker connected; dropping the previous one");
        let _ = previous.shutdown(std::net::Shutdown::Both);
    }
    shared.output.connect(connection, sender);

    std::thread::Builder::new()
        .name("carla-bridge:write".into())
        .spawn(move || {
            let mut writer = BufWriter::new(writer);
            // Ends once the connection is replaced or its reader finishes, after
            // writing whatever was already queued, such as a final error.
            while let Ok(message) = receiver.recv() {
                if let Err(err) = write(&mut writer, &receiver, message) {
                    tracing::warn!(%err, "worker write failed");
                    break;
                }
            }
            let _ = writer.get_ref().shutdown(std::net::Shutdown::Both);
        })
        .context("start the worker writer")?;

    let shared = shared.clone();
    std::thread::Builder::new()
        .name("carla-bridge:read".into())
        .spawn(move || {
            let mut connection = Connection {
                shared: &shared,
                key: connection,
                streams: HashMap::new(),
                hello: false,
            };
            let result = connection.serve(&mut BufReader::new(reader));
            if let Err(err) = &result {
                tracing::error!(err = format!("{err:#}"), "worker protocol error");
                shared.output.send(
                    connection.key,
                    &Event::Error {
                        id: 0,
                        message: format!("{err:#}"),
                    },
                );
            }
            connection.finish();
        })
        .context("start the worker reader")?;
    Ok(())
}

/// Write `message` and everything queued behind it, then flush once, so a
/// burst of packets costs one syscall rather than one each.
fn write(
    writer: &mut BufWriter<UnixStream>,
    queue: &mpsc::Receiver<Bytes>,
    message: Bytes,
) -> io::Result<()> {
    writer.write_all(&message)?;
    while let Ok(message) = queue.try_recv() {
        writer.write_all(&message)?;
    }
    writer.flush()
}

struct Connection<'a> {
    shared: &'a Arc<Shared>,
    key: u64,
    streams: HashMap<u32, Arc<Stream>>,
    hello: bool,
}

impl Connection<'_> {
    fn serve(&mut self, reader: &mut impl io::Read) -> Result<()> {
        while let Some((kind, body)) = protocol::read(reader).context("read from the worker")? {
            let request = Request::decode(kind, body).context("decode a worker request")?;
            self.handle(request)?;
        }
        Ok(())
    }

    fn handle(&mut self, request: Request) -> Result<()> {
        // A replaced connection's reader can still be mid-request. Holding the
        // connection lock orders each request against a replacement, and a
        // superseded connection may not touch the streams its successor owns.
        let current = self.shared.connection.lock();
        if current.as_ref().map(|(key, _)| *key) != Some(self.key) {
            bail!("superseded by a newer worker connection");
        }
        if !self.hello && !matches!(request, Request::Hello { .. }) {
            bail!("expected hello, got {request:?}");
        }
        match request {
            Request::Hello { version } => {
                if self.hello {
                    bail!("repeated hello");
                }
                if version != VERSION {
                    bail!("worker speaks protocol {version}, bridge speaks {VERSION}");
                }
                self.hello = true;
                // Sending waits for queue room; never while a replacement waits.
                drop(current);
                self.shared
                    .output
                    .send(self.key, &Event::Hello { version: VERSION });
            }
            Request::Open {
                id,
                name,
                renditions,
            } => {
                if id == 0 {
                    bail!("stream id 0 is reserved for the connection");
                }
                validate(&name, &renditions)?;
                if self.streams.contains_key(&id) {
                    bail!("stream {id} is already open");
                }
                if self.streams.values().any(|stream| stream.name() == name) {
                    bail!("stream {name:?} is already open");
                }
                let stream = self.shared.stream(&name)?;
                stream.open(Session {
                    key: self.session(id),
                    renditions,
                });
                self.streams.insert(id, stream);
            }
            Request::Demand { id, renditions } => {
                self.stream(id)?.demand(self.session(id), renditions)?
            }
            Request::Keyframe { id, rendition } => {
                self.stream(id)?.keyframe(self.session(id), rendition)?
            }
            Request::Close { id } => {
                let stream = self
                    .streams
                    .remove(&id)
                    .with_context(|| format!("stream {id} is not open"))?;
                stream.close(self.session(id));
            }
        }
        Ok(())
    }

    fn stream(&self, id: u32) -> Result<&Arc<Stream>> {
        self.streams
            .get(&id)
            .with_context(|| format!("stream {id} is not open"))
    }

    fn session(&self, id: u32) -> SessionId {
        SessionId {
            connection: self.key,
            id,
        }
    }

    /// The worker is gone: stop every stream it opened.
    fn finish(self) {
        self.shared.output.disconnect(self.key);
        for (id, stream) in self.streams {
            stream.close(SessionId {
                connection: self.key,
                id,
            });
        }
    }
}

fn validate(name: &str, renditions: &[protocol::Rendition]) -> Result<()> {
    if name.is_empty() {
        bail!("stream name is empty");
    }
    if renditions.is_empty() || renditions.len() > MAX_RENDITIONS {
        bail!("stream {name:?} has {} renditions", renditions.len());
    }
    for (index, rendition) in renditions.iter().enumerate() {
        let label = format!("stream {name:?} rendition {:?}", rendition.name);
        if rendition.name.is_empty() {
            bail!("{label}: empty name");
        }
        if renditions[..index]
            .iter()
            .any(|other| other.name == rendition.name)
        {
            bail!("{label}: duplicate name");
        }
        if rendition.width == 0
            || rendition.height == 0
            || !rendition.width.is_multiple_of(2)
            || !rendition.height.is_multiple_of(2)
        {
            bail!(
                "{label}: size {}x{} must be non-zero and even",
                rendition.width,
                rendition.height
            );
        }
        if rendition.framerate == 0 || rendition.bitrate == 0 {
            bail!("{label}: framerate and bitrate must be non-zero");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_superseded_connection_changes_nothing() {
        let path = std::env::temp_dir().join(format!(
            "carla-bridge-superseded-{}.sock",
            std::process::id()
        ));
        let bridge = crate::Bridge::listen(&path).unwrap();
        // Not the current connection: another worker replaced it (here, none
        // is current at all).
        let mut stale = Connection {
            shared: &bridge.shared,
            key: u64::MAX,
            streams: HashMap::new(),
            hello: true,
        };
        let err = stale
            .handle(Request::Open {
                id: 1,
                name: "truck1/front".into(),
                renditions: vec![protocol::Rendition {
                    name: "sd".into(),
                    width: 426,
                    height: 240,
                    bitrate: 800_000,
                    framerate: 30,
                }],
            })
            .unwrap_err();
        assert!(err.to_string().contains("superseded"), "{err:#}");
        assert!(bridge.shared.streams.lock().is_empty());
    }
}
