# Docker log emitter

A small Rust service for emitting container logs from Docker-compatible runtime engines in a syslog format.

### Features:

* Automatic reconnect to the Docker-compatible API socket on errors and engine restarts
* Automatic reconnect to remote emitters and log emission retrials without losing log lines on errors
* Automatic attach / detach of starting / stopping containers during runtime
* Support for log file rotation using `SIGHUP` POSIX signal

#### Available feature flags:

* `systemd`: Adds support for
  `systemd` [service notifications](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html#Type=)
  and direct logging to `systemd-journald`.
  Enabled by default
* `exec-by-pid`: Enables collection of information about running executables by querying the host process table.
  Enabled by default

#### Available environment variable options:

* `EMITTER_URL`: URL the collected, syslog formatted log data should get emitted to.
  Currently supported protocols: `tcp:`, `udp:` and `file:`. Required
* `DOCKER_HOST`: Override the default Docker API socket, Optional
* `PIDFILE`: Create a PID file after service startup. Optional
* `SYSLOG_RFC`: Specifies the syslog format to be used for emission.
  Currently supported: [RFC3164](https://datatracker.ietf.org/doc/html/rfc3164)
  and [RFC5424](https://datatracker.ietf.org/doc/html/rfc5424).
  Defaults to RFC5424
* `USE_EXEC_PID`: Set to `false` to disable `exec-by-pid` feature on runtime. Optional

#### Available container labels:

* `de.hammer065.docker-log-emitter.enabled`: Set to `false` to disable containers log collection
* `de.hammer065.docker-log-emitter.app_name`: Override the used executable name to be emitted in syslog lines 
