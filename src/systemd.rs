use lazy_static::lazy_static;
use libsystemd::daemon::NotifyState;

lazy_static! {
    pub static ref STARTED_WITH: bool = libsystemd::daemon::booted()
        && std::env::var("SYSTEMD_EXEC_PID").map_or_else(
            |_| std::env::var("NOTIFY_SOCKET").map_or(false, |ns| !ns.is_empty()),
            |pid_str| {
                pid_str
                    .parse()
                    .map_or(false, |pid: u32| pid == std::process::id())
            }
        );
}

pub fn notify(notify_state: &NotifyState) {
    if *STARTED_WITH {
        if let Err(err) = libsystemd::daemon::notify(false, &[notify_state.clone()]) {
            tracing::warn!("Could not notify systemd {notify_state:?}: {err}");
        }
    }
}
