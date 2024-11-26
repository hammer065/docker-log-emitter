use chrono::{DateTime, TimeZone};

#[allow(dead_code)]
pub enum Facility {
    Kernel,
    UserLevel,
    MailSystem,
    SystemDaemon,
    SecurityMessage,
    SyslogdInternal,
    LinePrinter,
    NetworkNews,
    Uucp,
    ClockDaemon,
    FtpDaemon,
    Ntp,
    LogAudit,
    LogAlert,
    Local0,
    Local1,
    Local2,
    Local3,
    Local4,
    Local5,
    Local6,
    Local7,
}

impl Facility {
    const fn numerical_code(&self) -> u16 {
        match self {
            Self::Kernel => 0,
            Self::UserLevel => 1,
            Self::MailSystem => 2,
            Self::SystemDaemon => 3,
            Self::SecurityMessage => 4,
            Self::SyslogdInternal => 5,
            Self::LinePrinter => 6,
            Self::NetworkNews => 7,
            Self::Uucp => 8,
            Self::ClockDaemon => 9,
            Self::FtpDaemon => 11,
            Self::Ntp => 12,
            Self::LogAudit => 13,
            Self::LogAlert => 14,
            Self::Local0 => 16,
            Self::Local1 => 17,
            Self::Local2 => 18,
            Self::Local3 => 19,
            Self::Local4 => 20,
            Self::Local5 => 21,
            Self::Local6 => 22,
            Self::Local7 => 23,
        }
    }
}

#[allow(dead_code)]
pub enum Severity {
    Emergency,
    Alert,
    Critical,
    Error,
    Warning,
    Notice,
    Informational,
    Debug,
}

impl Severity {
    const fn numerical_code(&self) -> u16 {
        match self {
            Self::Emergency => 0,
            Self::Alert => 1,
            Self::Critical => 2,
            Self::Error => 3,
            Self::Warning => 4,
            Self::Notice => 5,
            Self::Informational => 6,
            Self::Debug => 7,
        }
    }
}

pub enum Formatter {
    Rfc3164 {
        pri_offset: u16,
        hostname: String,
        procid: String,
    },
    Rfc5424 {
        pri_offset: u16,
        hostname: String,
        procid: String,
        msgid: String,
    },
}

impl Formatter {
    pub fn rfc3164(facility: &Facility, hostname: &str, pid: Option<i64>) -> Self {
        let procid = pid.map_or_else(String::new, |p| format!("[{p}]"));

        Self::Rfc3164 {
            pri_offset: facility.numerical_code() * 8,
            hostname: String::from(hostname),
            procid,
        }
    }

    pub fn rfc5424(
        facility: &Facility,
        hostname: &str,
        pid: Option<i64>,
        msgid: Option<&str>,
    ) -> Self {
        let hostname = if hostname.len() > 255 {
            &hostname[..255]
        } else {
            hostname
        };

        let procid = pid.map_or_else(|| "-".to_string(), |p| p.to_string());

        let msgid = msgid.map_or("-", |msgid| {
            if msgid.len() > 32 {
                &msgid[0..32]
            } else {
                msgid
            }
        });

        Self::Rfc5424 {
            pri_offset: facility.numerical_code() * 8,
            hostname: String::from(hostname),
            procid,
            msgid: String::from(msgid),
        }
    }

    pub fn format<Tz: TimeZone>(
        &self,
        msg: &[u8],
        app_name: Option<&str>,
        severity: &Severity,
        ts: &DateTime<Tz>,
    ) -> Vec<u8>
    where
        Tz::Offset: std::fmt::Display,
    {
        match self {
            Self::Rfc3164 {
                pri_offset,
                hostname,
                procid,
            } => {
                let pri = pri_offset + severity.numerical_code();
                let timestamp = ts.format("%b %e %H:%M:%S");
                let app_name = app_name.unwrap_or("-");

                let header = format!("<{pri}>{timestamp} {hostname} {app_name}{procid}: ");

                let mut data = header.into_bytes();
                data.extend(msg.iter().filter(|b| !matches!(**b, b'\n' | b'\r')));
                data.push(b'\n');
                data
            }
            Self::Rfc5424 {
                pri_offset,
                hostname,
                procid,
                msgid,
            } => {
                let pri = pri_offset + severity.numerical_code();
                let timestamp = ts.to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
                let app_name = app_name.map_or("-", |app_name| {
                    if app_name.len() > 48 {
                        &app_name[..48]
                    } else {
                        app_name
                    }
                });

                let header =
                    format!("<{pri}>1 {timestamp} {hostname} {app_name} {procid} {msgid} - ");

                let mut data = header.into_bytes();
                data.extend(msg.iter().filter(|b| !matches!(**b, b'\n' | b'\r')));
                data.push(b'\n');
                data
            }
        }
    }
}
