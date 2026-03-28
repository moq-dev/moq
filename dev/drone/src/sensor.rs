use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;

/// Shared battery level, updated by the game loop.
pub type BatteryLevel = Arc<AtomicU32>;

pub fn new_battery() -> BatteryLevel {
    Arc::new(AtomicU32::new(100))
}

/// Publishes sensor telemetry to the sensor track every ~1 second.
pub async fn run_sensor(
    mut producer: moq_lite::TrackProducer,
    battery: BatteryLevel,
) -> Result<()> {
    let start = tokio::time::Instant::now();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));

    loop {
        interval.tick().await;

        let uptime = start.elapsed().as_secs();
        let bat = battery.load(Ordering::Relaxed);

        // Generate fake but varying sensor data.
        let temp = 35.0 + (uptime % 20) as f64 * 0.5;
        let gps_lat = 37.7749 + (uptime as f64 * 0.0001).sin() * 0.001;
        let gps_lng = -122.4194 + (uptime as f64 * 0.00013).cos() * 0.001;

        let json = serde_json::json!({
            "battery": bat,
            "temp": temp,
            "gps": [gps_lat, gps_lng],
            "uptime": uptime,
        });

        let data = json.to_string();
        let mut group = producer.append_group()?;
        group.write_frame(data.into_bytes())?;
        group.finish()?;

        tracing::trace!(uptime, bat, temp, "published sensor data");
    }
}
