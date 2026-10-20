use std::collections::HashSet;
use std::net;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context};
use bytes::Bytes;
use clap::{Parser, Subcommand, ValueEnum};
use moq_native_ietf::quic;
use moq_transport::message::RequestErrorCode;
use moq_transport::{
    coding::TrackNamespace,
    serve::{Datagram, ServeError, Track, TrackReader, TrackReaderMode, Tracks},
    session::{Publisher, SessionError, Subscriber},
};
use url::Url;

const MAGIC: &[u8; 8] = b"MOQSMOKE";
const HEADER_SIZE: usize = 20;

#[derive(Parser)]
#[command(about = "Sustained cloudflare/moq-rs interoperability smoke client")]
struct Args {
    /// Relay URL (https:// for WebTransport, moqt:// for raw QUIC).
    #[arg(long)]
    url: Url,

    /// Namespace containing the smoke track.
    #[arg(long)]
    namespace: String,

    /// Track carrying deterministic smoke payloads.
    #[arg(long, default_value = "payload")]
    track: String,

    /// Listen address for the client's UDP socket.
    #[arg(long, default_value = "[::]:0")]
    bind: net::SocketAddr,

    /// Danger: disable TLS certificate verification for the local smoke relay.
    #[arg(long)]
    tls_disable_verify: bool,

    /// Root certificate used to avoid dependence on the host trust store.
    #[arg(long)]
    tls_root: Option<PathBuf>,

    /// Object delivery mode to exercise.
    #[arg(long, value_enum, default_value_t = Delivery::Subgroup)]
    delivery: Delivery,

    /// Bytes in each deterministic object.
    #[arg(long)]
    object_size: usize,

    /// Delay between published objects.
    #[arg(long, default_value_t = 5)]
    interval_ms: u64,

    /// Delay before publishing the first object, allowing relay registration and subscription.
    #[arg(long, default_value_t = 1_500)]
    start_delay_ms: u64,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Delivery {
    Subgroup,
    Datagram,
}

#[derive(Subcommand)]
enum Command {
    /// Publish deterministic objects until the orchestrator stops the process.
    Publish,
    /// Exit 0 once the relay accepts the subscription, or 75 while its namespace is unknown.
    Probe,
    /// Subscribe and validate the requested number of complete objects.
    Subscribe {
        #[arg(long, default_value_t = 32)]
        objects: usize,
    },
}

enum Outcome {
    Complete,
    NamespaceUnavailable,
}

fn retryable_probe_request_error(err: &ServeError) -> bool {
    matches!(err, ServeError::NotFound | ServeError::NotFoundWithId(_, _))
        || matches!(
            err,
            ServeError::Closed(code) if *code == RequestErrorCode::DoesNotExist as u64
        )
}

fn retryable_probe_session_error(err: &SessionError) -> bool {
    matches!(err, SessionError::Serve(ServeError::Cancel))
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match run().await {
        Ok(Outcome::Complete) => ExitCode::SUCCESS,
        Ok(Outcome::NamespaceUnavailable) => ExitCode::from(75),
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<Outcome> {
    let args = Args::parse();
    anyhow::ensure!(
        args.object_size >= HEADER_SIZE,
        "object size must be at least {HEADER_SIZE} bytes"
    );
    anyhow::ensure!(
        args.object_size <= u32::MAX as usize,
        "object size exceeds the encoded length field"
    );
    if let Command::Subscribe { objects } = &args.command {
        anyhow::ensure!(*objects > 0, "subscriber must validate at least one object");
    }

    let tls = moq_native_ietf::tls::Args {
        disable_verify: args.tls_disable_verify,
        root: args.tls_root.clone().into_iter().collect(),
        ..Default::default()
    }
    .load()?;
    let endpoint = quic::Endpoint::new(quic::Config::new(args.bind, None, tls)?)?;
    let (session, _, transport) = endpoint.client.connect(&args.url, None).await?;

    match args.command {
        Command::Publish => publish(args, session, transport)
            .await
            .map(|_| Outcome::Complete),
        Command::Probe => probe(args, session, transport).await,
        Command::Subscribe { objects } => subscribe(args, objects, session, transport)
            .await
            .map(|_| Outcome::Complete),
    }
}

async fn publish(
    args: Args,
    session: web_transport::Session,
    transport: moq_transport::session::Transport,
) -> anyhow::Result<()> {
    let (session, mut publisher) = Publisher::connect(session, transport)
        .await
        .context("publisher SETUP failed")?;
    let namespace = TrackNamespace::from_utf8_path(&args.namespace);
    let (mut tracks, _, tracks_reader) = Tracks::new(namespace).produce();
    let track = tracks
        .create(&args.track)
        .context("failed to create smoke track")?;

    let send = async move {
        // Give the namespace advertisement and downstream subscription time to
        // reach the publisher before opening the first group. This avoids making
        // the smoke result depend on how a relay handles joining a partial group.
        tokio::time::sleep(Duration::from_millis(args.start_delay_ms)).await;
        match args.delivery {
            Delivery::Subgroup => {
                publish_subgroups(track, args.object_size, args.interval_ms).await
            }
            Delivery::Datagram => {
                publish_datagrams(track, args.object_size, args.interval_ms).await
            }
        }
    };

    tokio::select! {
        res = session.run() => res.context("publisher session failed"),
        res = publisher.publish_namespace(tracks_reader) => res.context("namespace publication failed"),
        res = send => res,
    }
}

async fn publish_subgroups(
    track: moq_transport::serve::TrackWriter,
    object_size: usize,
    interval_ms: u64,
) -> anyhow::Result<()> {
    let mut groups = track.subgroups()?;
    let mut sequence = 0_u64;
    loop {
        let mut group = groups.append(127)?;
        for _ in 0..8 {
            group.write(payload(sequence, object_size))?;
            sequence = sequence.wrapping_add(1);
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    }
}

async fn publish_datagrams(
    track: moq_transport::serve::TrackWriter,
    object_size: usize,
    interval_ms: u64,
) -> anyhow::Result<()> {
    let mut datagrams = track.datagrams()?;
    let mut sequence = 0_u64;
    loop {
        datagrams.write(Datagram {
            group_id: sequence,
            object_id: 0,
            priority: 127,
            payload: payload(sequence, object_size),
            extension_headers: Default::default(),
        })?;
        sequence = sequence.wrapping_add(1);
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
    }
}

async fn subscribe(
    args: Args,
    objects: usize,
    session: web_transport::Session,
    transport: moq_transport::session::Transport,
) -> anyhow::Result<()> {
    let (session, mut subscriber) = Subscriber::connect(session, transport)
        .await
        .context("subscriber SETUP failed")?;
    let namespace = TrackNamespace::from_utf8_path(&args.namespace);
    let (track_writer, track_reader) = Track::new(namespace, args.track).produce();

    let receive = receive(track_reader, args.delivery, args.object_size, objects);
    let accept_namespaces = accept_namespaces(subscriber.clone());
    tokio::select! {
        res = session.run() => {
            res.context("subscriber session failed")?;
            bail!("subscriber session ended before validation completed")
        },
        res = subscriber.subscribe(track_writer) => {
            res.context("subscription ended before validation completed")?;
            bail!("subscription ended before validation completed")
        },
        res = receive => res,
        res = accept_namespaces => res,
    }
}

async fn probe(
    args: Args,
    session: web_transport::Session,
    transport: moq_transport::session::Transport,
) -> anyhow::Result<Outcome> {
    let (session, mut subscriber) = Subscriber::connect(session, transport)
        .await
        .context("probe SETUP failed")?;
    let namespace = TrackNamespace::from_utf8_path(&args.namespace);
    let (track_writer, _) = Track::new(namespace, args.track).produce();
    let accept_namespaces = accept_namespaces(subscriber.clone());

    tokio::select! {
        res = session.run() => match res {
            // An early SUBSCRIBE can race PUBLISH_NAMESPACE. Depending on which
            // side closes first, moq-rs surfaces that rejection either on the
            // request or as a cancelled probe session. Both mean "retry the
            // readiness probe"; the publisher-liveness/deadline checks remain
            // in cloudflare.sh.
            Err(err) if retryable_probe_session_error(&err) => Ok(Outcome::NamespaceUnavailable),
            res => {
                res.context("probe session failed")?;
                bail!("probe session ended before subscription acceptance")
            }
        },
        res = subscriber.subscribe_open(track_writer) => match res {
            Ok(_subscription) => {
                println!("subscription accepted");
                Ok(Outcome::Complete)
            },
            Err(err) if retryable_probe_request_error(&err) => {
                Ok(Outcome::NamespaceUnavailable)
            },
            Err(err) => Err(err).context("probe subscription failed"),
        },
        res = accept_namespaces => {
            res?;
            bail!("probe namespace acceptance loop ended")
        },
    }
}

async fn accept_namespaces(mut subscriber: Subscriber) -> anyhow::Result<()> {
    let mut active = Vec::new();
    loop {
        let mut namespace = subscriber
            .published_namespace()
            .await
            .context("published namespace queue closed")?;
        namespace
            .ok()
            .context("failed to accept published namespace")?;
        active.push(namespace);
    }
}

async fn receive(
    track: TrackReader,
    delivery: Delivery,
    object_size: usize,
    wanted: usize,
) -> anyhow::Result<()> {
    let mut seen = HashSet::with_capacity(wanted);
    match (delivery, track.mode().await?) {
        (Delivery::Subgroup, TrackReaderMode::Subgroups(mut groups)) => {
            while seen.len() < wanted {
                let mut group = groups
                    .next()
                    .await?
                    .context("subgroup stream ended before validation completed")?;
                while let Some(object) = group.read_next().await? {
                    let sequence = validate(&object, object_size)?;
                    seen.insert(sequence);
                    if seen.len() == wanted {
                        break;
                    }
                }
            }
        }
        (Delivery::Datagram, TrackReaderMode::Datagrams(mut datagrams)) => {
            while seen.len() < wanted {
                let datagram = datagrams
                    .read()
                    .await?
                    .context("datagram stream ended before validation completed")?;
                seen.insert(validate(&datagram.payload, object_size)?);
            }
        }
        (expected, _) => bail!("relay returned a different delivery mode than {expected:?}"),
    }

    println!(
        "validated {} {} objects ({} bytes)",
        seen.len(),
        match delivery {
            Delivery::Subgroup => "subgroup",
            Delivery::Datagram => "datagram",
        },
        seen.len() * object_size
    );
    Ok(())
}

fn payload(sequence: u64, size: usize) -> Bytes {
    let mut payload = vec![0; size];
    payload[..8].copy_from_slice(MAGIC);
    payload[8..16].copy_from_slice(&sequence.to_be_bytes());
    payload[16..20].copy_from_slice(&(size as u32).to_be_bytes());
    for (offset, byte) in payload[HEADER_SIZE..].iter_mut().enumerate() {
        *byte = pattern(sequence, offset);
    }
    payload.into()
}

fn validate(payload: &[u8], expected_size: usize) -> anyhow::Result<u64> {
    anyhow::ensure!(payload.len() == expected_size, "object size mismatch");
    anyhow::ensure!(&payload[..8] == MAGIC, "object magic mismatch");
    let sequence = u64::from_be_bytes(payload[8..16].try_into().unwrap());
    let declared_size = u32::from_be_bytes(payload[16..20].try_into().unwrap()) as usize;
    anyhow::ensure!(
        declared_size == expected_size,
        "declared object size mismatch"
    );
    for (offset, byte) in payload[HEADER_SIZE..].iter().enumerate() {
        anyhow::ensure!(
            *byte == pattern(sequence, offset),
            "object payload corruption"
        );
    }
    Ok(sequence)
}

fn pattern(sequence: u64, offset: usize) -> u8 {
    sequence.to_be_bytes()[offset % 8] ^ (offset as u8).wrapping_mul(17)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trip() {
        let payload = payload(42, 65_536);
        assert_eq!(validate(&payload, 65_536).unwrap(), 42);
    }

    #[test]
    fn corruption_is_rejected() {
        let mut payload = payload(7, 900).to_vec();
        payload[100] ^= 1;
        assert!(validate(&payload, 900).is_err());
    }

    #[test]
    fn sequence_header_corruption_is_rejected() {
        let mut payload = payload(7, 900).to_vec();
        payload[8] ^= 1;
        assert!(validate(&payload, 900).is_err());
    }

    #[test]
    fn readiness_race_errors_are_retryable() {
        assert!(retryable_probe_session_error(&SessionError::Serve(
            ServeError::Cancel
        )));
        assert!(retryable_probe_request_error(&ServeError::NotFound));
        assert!(retryable_probe_request_error(&ServeError::Closed(
            RequestErrorCode::DoesNotExist as u64
        )));
    }

    #[test]
    fn unrelated_probe_errors_are_not_retryable() {
        assert!(!retryable_probe_session_error(&SessionError::Internal));
        assert!(!retryable_probe_request_error(&ServeError::Closed(
            RequestErrorCode::Unauthorized as u64
        )));
        assert!(!retryable_probe_request_error(&ServeError::Internal(
            "test".into()
        )));
    }
}
