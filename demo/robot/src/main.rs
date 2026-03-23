use std::path::PathBuf;

use clap::Parser;
use url::Url;

mod robot;
mod sensor;
mod video;

#[derive(Parser, Clone)]
pub struct Config {
    /// Connect to the given relay URL.
    #[arg(long)]
    pub url: Url,

    /// The robot ID (used in broadcast path: robot/{id}).
    #[arg(long)]
    pub id: String,

    /// Video file paths for each camera angle.
    #[arg(long, required = true, num_args = 1..)]
    pub angles: Vec<PathBuf>,

    /// The MoQ client configuration.
    #[command(flatten)]
    pub client: moq_native::ClientConfig,

    /// The log configuration.
    #[command(flatten)]
    pub log: moq_native::Log,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    config.log.init();

    let client = config.client.clone().init()?;
    let robot = robot::Robot::new(&config);

    // Create an origin for both publishing and consuming.
    let origin = moq_lite::Origin::produce();

    // Publish the robot broadcast.
    let broadcast_path = format!("robot/{}", config.id);
    origin.publish_broadcast(&broadcast_path, robot.consume());

    tracing::info!(url = %config.url, id = %config.id, "connecting to relay");

    // Connect with both publish and consume permissions.
    let mut session = client
        .with_publish(origin.consume())
        .with_consume(origin.clone())
        .connect(config.url.clone())
        .await?;

    // Subscribe to viewer announcements.
    let viewer_prefix = format!("robot/{}/viewer", config.id);
    let viewer_prefix_path: moq_lite::Path<'_> = viewer_prefix.as_str().into();
    let mut viewer_origin = origin
        .consume_only(&[viewer_prefix_path])
        .expect("viewer prefix should be valid");

    tokio::select! {
        res = robot.run() => res,
        res = session.closed() => res.map_err(Into::into),
        res = robot::handle_viewers(&mut viewer_origin, &robot) => res,
        _ = tokio::signal::ctrl_c() => {
            session.close(moq_lite::Error::Cancel);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            Ok(())
        },
    }
}
