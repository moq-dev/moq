//! Finite raw-track interop client. The harness acknowledges a clean read on stdin.
use std::time::Duration;

use anyhow::{Context, ensure};
use moq_tokio::moq_net;
use tokio::io::AsyncBufReadExt;

const GROUPS: u64 = 4;
const BYTES: usize = 256 * 1024;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 4,
        "usage: interop-tail publish|subscribe URL BROADCAST"
    );
    let url: url::Url = args[2].parse()?;
    let mut config = moq_tokio::connect::Config::default();
    config.websocket.enabled = Some(false);
    let client = config.init(Default::default())?.with_reconnect(false);
    let origin = moq_tokio::origin::spawn();

    match args[1].as_str() {
        "publish" => {
            let broadcast = origin.create_broadcast(&args[3])?;
            let mut track = broadcast.create_track("tail", None)?;
            broadcast.announce(Default::default())?;
            let _session = client
                .with_publisher(&origin)
                .connect(url)
                .established()
                .await?;
            track.demand().used().await?;
            // Declare the end before the final group's payload is even available.
            track.finish_at(GROUPS)?;
            for sequence in 0..GROUPS {
                let mut group = track.append_group()?;
                group.write_frame(moq_net::Timestamp::ZERO, vec![sequence as u8; BYTES])?;
                group.finish()?;
            }
            // A finished track is not a drained transport. Keep the session alive
            // until the harness confirms the subscriber checked every byte and EOF.
            let mut ack = String::new();
            tokio::io::BufReader::new(tokio::io::stdin())
                .read_line(&mut ack)
                .await?;
            ensure!(
                ack == "clean end\n",
                "reader did not acknowledge a clean end"
            );
            broadcast.finish();
            eprintln!("tail acknowledged");
        }
        "subscribe" => {
            let consumer = origin.consume();
            let _session = client
                .with_subscriber(origin)
                .connect(url)
                .established()
                .await?;
            let broadcast = consumer.routed_broadcast(&args[3]).await?;
            let options = moq_net::track::Subscription::default()
                .with_max_age(Duration::from_secs(5))
                .with_groups(0..);
            let mut track = broadcast.track("tail")?.subscribe(Some(options)).await?;
            let mut seen = Vec::new();
            while let Some(mut group) = track.recv_group().await? {
                let sequence = group.sequence;
                let frame = group.read_frame().await?.context("empty group")?;
                ensure!(sequence < GROUPS, "unexpected group {sequence}");
                ensure!(frame.payload.len() == BYTES, "truncated group {sequence}");
                ensure!(
                    frame.payload.iter().all(|byte| *byte == sequence as u8),
                    "corrupt group {sequence}"
                );
                ensure!(
                    group.read_frame().await?.is_none(),
                    "extra frame in group {sequence}"
                );
                seen.push(sequence);
            }
            seen.sort_unstable();
            ensure!(
                seen == (0..GROUPS).collect::<Vec<_>>(),
                "wrong groups: {seen:?}"
            );
            ensure!(track.finished().await? == GROUPS, "wrong declared end");
            eprintln!("tail clean end=4 groups=0,1,2,3 bytes=1048576");
        }
        _ => anyhow::bail!("unknown role: {}", args[1]),
    }
    Ok(())
}
