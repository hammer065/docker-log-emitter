#[cfg(all(feature = "systemd", target_os = "linux"))]
use crate::systemd;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

pub fn init() {
    let env_filter = EnvFilter::from_default_env();

    let registry = tracing_subscriber::registry();

    #[cfg(all(feature = "systemd", target_os = "linux"))]
    {
        if !*systemd::STARTED_WITH {
            registry
                .with(tracing_subscriber::fmt::layer().with_filter(env_filter))
                .init();
            return;
        }

        match tracing_journald::layer() {
            Ok(layer) => {
                registry.with(layer.with_filter(env_filter)).init();
            }
            // journald is typically available on Linux systems, but nowhere else.
            // Portable software should handle its absence gracefully.
            Err(err) => {
                registry
                    .with(tracing_subscriber::fmt::layer().with_filter(env_filter))
                    .init();
                tracing::warn!("Could not connect to journald: {err}");
            }
        }
    }
    #[cfg(not(all(feature = "systemd", target_os = "linux")))]
    {
        registry
            .with(tracing_subscriber::fmt::layer().with_filter(env_filter))
            .init();
    }
}
