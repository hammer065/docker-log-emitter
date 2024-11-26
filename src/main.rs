use bollard::system::EventsOptions;
use bollard::Docker;
use futures_util::StreamExt;
use lazy_static::lazy_static;
#[cfg(all(target_os = "linux", feature = "systemd"))]
use libsystemd::daemon::NotifyState;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

mod container_logs;
mod emitter;
mod helpers;
mod logging;
mod syslog;
#[cfg(all(feature = "systemd", target_os = "linux"))]
mod systemd;

pub type EmitterData = Vec<u8>;

// Constants
lazy_static! {
    static ref EVENT_FILTER: HashMap<&'static str, Vec<&'static str>> = {
        let mut event_filter = HashMap::with_capacity(2);
        event_filter.insert("type", vec!["container"]);
        event_filter.insert("event", vec!["start"]);

        event_filter
    };
    static ref HOSTNAME: String = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "localhost".to_string());
}

const ONE_SECOND: Duration = Duration::from_secs(1);

// Initializer functions
#[inline]
fn ctrl_c_handler(token: CancellationToken, tracker: &TaskTracker) {
    tracker.spawn(async move {
        tokio::select! {
            biased;
            () = token.cancelled() => (),
            result = tokio::signal::ctrl_c() => {
                match result {
                    Ok(()) => {}
                    Err(err) => {
                        // we also shut down in case of error
                        tracing::error!("Unable to listen for shutdown signal: {err}");
                    }
                }
                token.cancel();
            }
        }
    });
}

struct PidFile {
    path: Option<PathBuf>,
}

impl PidFile {
    pub fn new() -> Self {
        let path = std::env::var("PIDFILE")
            .map(PathBuf::from)
            .map_or(None, |pid_file| {
                match std::fs::write(&pid_file, format!("{}\n", std::process::id())) {
                    Ok(()) => Some(pid_file),
                    Err(err) => {
                        tracing::warn!("Unable to write PID file: {err}");
                        None
                    }
                }
            });

        Self { path }
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if let Some(pid_file) = self.path.take() {
            if let Err(err) = std::fs::remove_file(pid_file) {
                tracing::warn!("Unable to remove PID file: {err}");
            }
        }
    }
}

#[inline]
fn emitter(
    rx: Receiver<EmitterData>,
    cancellation_token: CancellationToken,
    tracker: &TaskTracker,
) -> bool {
    let Ok(url) = std::env::var("EMITTER_URL") else {
        tracing::error!("Could not get EMITTER_URL environment variable");
        return false;
    };

    match emitter::start(url, cancellation_token, rx) {
        Ok(task) => {
            tracker.spawn(task);
        }
        Err(err) => {
            tracing::error!("Error starting emitter: {err}");
            return false;
        }
    }

    true
}

// Helper functions
async fn stop_execution(token: &CancellationToken, tracker: &TaskTracker) {
    token.cancel();
    tracker.close();
    tokio::select! {
        biased;
        () = tracker.wait() => {},
        // Timeout after one minute
        () = tokio::time::sleep(Duration::from_secs(60)) => {},
    }
}

// Main
#[tokio::main]
async fn main() {
    logging::init();
    tracing::info!("Starting application...");
    let mut is_starting_up = true;

    let pid_file = PidFile::new();

    let global_tracker = TaskTracker::new();

    let ctrl_c_token = CancellationToken::new();
    ctrl_c_handler(ctrl_c_token.clone(), &global_tracker);

    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<EmitterData>(1024);
    if !emitter(log_rx, ctrl_c_token.clone(), &global_tracker) {
        return;
    }

    'main_loop: loop {
        if ctrl_c_token.is_cancelled() {
            break;
        }

        let cancellation_token = CancellationToken::new();
        let tracker = TaskTracker::new();

        let docker = match Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(err) => {
                tracing::warn!("Could not connect to Docker daemon: {err}");
                tokio::time::sleep(ONE_SECOND).await;
                continue;
            }
        };

        let containers = match docker.list_containers::<String>(None).await {
            Ok(containers) => containers,
            Err(err) => {
                tracing::warn!("Could not fetch list of containers: {err}");
                tokio::time::sleep(ONE_SECOND).await;
                continue;
            }
        };
        for c in containers {
            let Some(container_id) = c.id else {
                continue;
            };

            tracker.spawn(container_logs::collect(
                container_id,
                log_tx.clone(),
                cancellation_token.clone(),
                HOSTNAME.as_str(),
            ));
        }

        if is_starting_up {
            #[cfg(all(target_os = "linux", feature = "systemd"))]
            systemd::notify(&NotifyState::Ready);
            tracing::info!("Application started successfully!");
            is_starting_up = false;
        }

        let events = &mut docker.events(Some(EventsOptions {
            since: None,
            until: None,
            filters: EVENT_FILTER.clone(),
        }));

        loop {
            tokio::select! {
                biased;
                () = ctrl_c_token.cancelled() => {
                    #[cfg(all(target_os = "linux", feature = "systemd"))]
                    systemd::notify(&NotifyState::Stopping);
                    tracing::info!("Shutting down...");
                    stop_execution(&cancellation_token, &tracker).await;
                    break 'main_loop;
                },
                event = events.next() => {
                    match event {
                        Some(Ok(event)) => {
                            let Some(container_id) = event.actor.and_then(|actor| actor.id) else {
                                continue;
                            };

                            tracker.spawn(container_logs::collect(container_id, log_tx.clone(), cancellation_token.clone(), HOSTNAME.as_str()));
                        },
                        Some(Err(err)) => {
                            tracing::warn!("Error while reading event stream: {err}");
                            stop_execution(&cancellation_token, &tracker).await;
                            break;
                        },
                        None => {
                            stop_execution(&cancellation_token, &tracker).await;
                            break;
                        }
                    };
                }
            }
        }
    }

    stop_execution(&ctrl_c_token, &global_tracker).await;
    drop(pid_file);
    tracing::info!("Completed shutdown. Bye!");
}
