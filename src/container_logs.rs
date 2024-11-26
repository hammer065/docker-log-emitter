use crate::syslog::{Facility, Formatter, Severity};
use crate::{helpers, EmitterData, ONE_SECOND};
use bollard::container::{LogOutput, LogsOptions};
use bollard::models::{ContainerConfig, ContainerInspectResponse, ContainerState};
use bollard::Docker;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use lazy_static::lazy_static;
use std::collections::HashMap;
#[cfg(feature = "exec-by-pid")]
use std::ffi::OsStr;
#[cfg(feature = "exec-by-pid")]
use std::path::Path;
use std::time::SystemTime;
#[cfg(feature = "exec-by-pid")]
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

lazy_static! {
    static ref EMPTY_STRING_HASHMAP: HashMap<String, String> = HashMap::new();
    static ref USE_RFC_3164: bool = std::env::var("SYSLOG_RFC")
        .map(|v| v == "3164")
        .unwrap_or(false);
    static ref USE_EXEC_PID: bool =
        std::env::var("USE_EXEC_PID").map_or(true, |v| helpers::bool_from_str(v.as_str()));
}

#[cfg(feature = "exec-by-pid")]
struct ExecByPid {
    system: System,
    pid: Pid,
    last_update: SystemTime,
    app_name: Option<String>,
    fallback: Option<String>,
}

#[cfg(feature = "exec-by-pid")]
impl ExecByPid {
    pub fn new(pid: Pid, fallback: Option<String>) -> Self {
        Self {
            system: System::new(),
            pid,
            last_update: SystemTime::UNIX_EPOCH,
            app_name: None,
            fallback,
        }
    }

    pub fn app_name(&mut self) -> Option<&str> {
        let now = SystemTime::now();
        if now
            .duration_since(self.last_update)
            .map(|d| d > ONE_SECOND)
            .unwrap_or(true)
        {
            self.system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[self.pid]),
                true,
                ProcessRefreshKind::new()
                    .with_exe(UpdateKind::Always)
                    .with_cmd(UpdateKind::Always),
            );
            self.last_update = now;
            let process = self.system.process(self.pid);
            self.app_name = process
                .and_then(Process::exe)
                .and_then(Path::file_name)
                .and_then(OsStr::to_str)
                .map(String::from)
                .or_else(|| {
                    process
                        .map(Process::cmd)
                        .and_then(|cmd| cmd.first())
                        .and_then(|first_cmd| first_cmd.to_str())
                        .map(helpers::file_name_from_str)
                })
                .or_else(|| self.fallback.clone());
        }

        self.app_name.as_deref()
    }
}

fn exec_by_container_info(
    container_path: Option<&str>,
    container_name: Option<&str>,
) -> Option<String> {
    container_path
        .map(helpers::file_name_from_str)
        .or_else(|| container_name.map(String::from))
}

fn parse_log_line(line: &[u8]) -> Option<(DateTime<Utc>, &[u8])> {
    let line = line.strip_suffix(b"\n\r").unwrap_or(line);
    let mut parts = line.splitn(2, |b| *b == b' ');

    match (parts.next(), parts.next()) {
        (Some(date), Some(msg)) => {
            let date = std::str::from_utf8(date)
                .ok()
                .map(DateTime::parse_from_rfc3339)
                .and_then(Result::ok)
                .map_or_else(Utc::now, |dt| dt.to_utc());
            Some((date, msg))
        }
        (Some(msg), None) => Some((Utc::now(), msg)),
        (None, _) => None,
    }
}

async fn handle_log_line(
    line: LogOutput,
    formatter: &Formatter,
    tx: &Sender<EmitterData>,
    static_app_name: Option<&str>,
    #[cfg(feature = "exec-by-pid")] exec_by_pid: Option<&mut ExecByPid>,
) -> Option<i64> {
    let (is_err, message) = match line {
        LogOutput::StdErr { message } => (true, message),
        LogOutput::StdOut { message }
        | LogOutput::StdIn { message }
        | LogOutput::Console { message } => (false, message),
    };
    let (ts, msg) = parse_log_line(message.as_ref())?;

    let severity = if is_err {
        &Severity::Error
    } else {
        &Severity::Informational
    };
    #[cfg(feature = "exec-by-pid")]
    let app_name = static_app_name.or_else(|| exec_by_pid.and_then(ExecByPid::app_name));
    #[cfg(not(feature = "exec-by-pid"))]
    let app_name = static_app_name;

    let data = formatter.format(msg, app_name, severity, &ts);

    if let Err(err) = tx.send(data).await {
        tracing::error!("Failed to queue log message: {}", err);
    };

    Some(ts.timestamp())
}

fn container_infos(
    container_info: &ContainerInspectResponse,
) -> (Option<String>, Option<i64>, &HashMap<String, String>, bool) {
    let container_name = container_info
        .name
        .as_deref()
        // Container names start with "/" per default
        .map(|n| String::from(n.strip_prefix('/').unwrap_or(n)));

    let pid = match container_info.state {
        Some(ContainerState { pid: Some(pid), .. }) => Some(pid),
        _ => None,
    };

    let labels = match container_info.config {
        Some(ContainerConfig {
            labels: Some(ref labels),
            ..
        }) => labels,
        _ => &*EMPTY_STRING_HASHMAP,
    };

    let enabled = labels
        .get("de.hammer065.docker-log-emitter.enabled")
        .map(String::as_str)
        .map_or(true, helpers::bool_from_str);

    (container_name, pid, labels, enabled)
}

#[inline]
fn get_formatter(
    facility: &Facility,
    hostname: &str,
    pid: Option<i64>,
    msgid: Option<&str>,
) -> Formatter {
    if *USE_RFC_3164 {
        Formatter::rfc3164(facility, hostname, pid)
    } else {
        Formatter::rfc5424(facility, hostname, pid, msgid)
    }
}

#[cfg(feature = "exec-by-pid")]
fn get_exec_pid(
    pid: Option<i64>,
    container_path: Option<&str>,
    container_name: Option<&str>,
) -> Option<ExecByPid> {
    if !*USE_EXEC_PID {
        return None;
    }

    pid.and_then(|p| usize::try_from(p).ok())
        .map(Pid::from)
        .map(|p| ExecByPid::new(p, exec_by_container_info(container_path, container_name)))
}

pub async fn collect(
    container_id: String,
    tx: Sender<EmitterData>,
    cancellation_token: CancellationToken,
    hostname: &str,
) {
    tracing::trace!("collect(container_id = \"{container_id}\") start");
    let cid_ref = container_id.as_str();
    let mut since = helpers::current_timestamp();

    'outer_loop: loop {
        if cancellation_token.is_cancelled() {
            tracing::trace!("collect(container_id = \"{container_id}\") end");
            return;
        }

        let docker = match Docker::connect_with_defaults() {
            Ok(docker) => docker,
            Err(err) => {
                tracing::warn!(
                    "Error connecting to Docker for container \"{container_id}\": {err}"
                );
                break;
            }
        };

        let container_info = match docker.inspect_container(cid_ref, None).await {
            Ok(info) => info,
            Err(err) => {
                tracing::warn!("Error fetching info for container \"{container_id}\": {err}");
                break;
            }
        };
        let (container_name, pid, labels, enabled) = container_infos(&container_info);

        if !enabled {
            tracing::info!("Disabled logging for container \"{container_id}\"");
            tracing::trace!("collect(container_id = \"{container_id}\") end");
            return;
        }

        let formatter = get_formatter(
            &Facility::SystemDaemon,
            hostname,
            pid,
            container_name.as_deref(),
        );

        let mut static_app_name = labels
            .get("de.hammer065.docker-log-emitter.app_name")
            .map(String::from);
        if cfg!(not(feature = "exec-by-pid")) || !*USE_EXEC_PID {
            static_app_name = static_app_name.or_else(|| {
                exec_by_container_info(container_info.path.as_deref(), container_name.as_deref())
            });
        }

        #[cfg(feature = "exec-by-pid")]
        let mut exec_by_pid = get_exec_pid(
            pid,
            container_info.path.as_deref(),
            container_name.as_deref(),
        );

        let logs = &mut docker.logs(
            cid_ref,
            Some(LogsOptions {
                follow: true,
                stdout: true,
                stderr: true,
                since,
                until: 0,
                timestamps: true,
                tail: "all",
            }),
        );

        tracing::info!("Attached to container \"{container_id}\"");
        tracing::trace!("collect(container_id = \"{container_id}\") loop");
        loop {
            tokio::select! {
                biased;
                () = cancellation_token.cancelled() => break 'outer_loop,
                result = logs.next() => {
                    match result {
                        Some(Ok(line)) => {
                            if let Some(ts) = handle_log_line(line, &formatter, &tx,
                                static_app_name.as_deref(), #[cfg(feature = "exec-by-pid")] {exec_by_pid.as_mut()}
                            ).await {
                                since = ts;
                            }
                        },
                        Some(Err(err)) => {
                            tracing::warn!("Error while reading log stream of container \"{container_id}\": {err}");
                            break;
                        },
                        None => break 'outer_loop,
                    }
                }
            }
        }
    }

    tracing::info!("Detached from container \"{container_id}\"");
    tracing::trace!("collect(container_id = \"{container_id}\") end");
}
