use crate::{EmitterData, ONE_SECOND};
use std::future::{pending, Future};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::signal::unix::SignalKind;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use tracing::log;

const ZERO_V4: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
const ZERO_V6: SocketAddr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);

const MAX_UDP_PACKET_SIZE: usize = 65_507;

struct SocketOptions<T> {
    addr: SocketAddr,
    socket: Option<T>,
}

enum SocketSender {
    Tcp(SocketOptions<TcpStream>),
    Udp(SocketOptions<UdpSocket>),
}

impl SocketSender {
    pub const fn tcp(addr: SocketAddr) -> Self {
        Self::Tcp(SocketOptions { addr, socket: None })
    }
    pub const fn udp(addr: SocketAddr) -> Self {
        Self::Udp(SocketOptions { addr, socket: None })
    }

    async fn connect(&mut self) {
        tracing::trace!("SocketSender::connect() start");
        match self {
            Self::Tcp(options) => loop {
                if options.socket.is_some() {
                    tracing::trace!("SocketSender::connect() end");
                    return;
                }

                let socket = match TcpStream::connect(options.addr).await {
                    Ok(socket) => socket,
                    Err(err) => {
                        tracing::warn!("Error connecting socket: {err}");
                        tokio::time::sleep(ONE_SECOND).await;
                        continue;
                    }
                };

                options.socket = Some(socket);
                break;
            },
            Self::Udp(options) => loop {
                if options.socket.is_some() {
                    tracing::trace!("SocketSender::connect() end");
                    return;
                }

                let socket = match UdpSocket::bind(if options.addr.is_ipv4() {
                    ZERO_V4
                } else {
                    ZERO_V6
                })
                .await
                {
                    Ok(socket) => socket,
                    Err(err) => {
                        tracing::warn!("Error building socket: {err}");
                        tokio::time::sleep(ONE_SECOND).await;
                        continue;
                    }
                };

                if let Err(err) = socket.connect(options.addr).await {
                    tracing::warn!("Error connecting socket: {err}");
                    tokio::time::sleep(ONE_SECOND).await;
                    continue;
                }

                options.socket = Some(socket);
                break;
            },
        }
        tracing::trace!("SocketSender::connect() end");
    }

    fn disconnect(&mut self) {
        tracing::trace!("SocketSender::disconnect() start");
        match self {
            Self::Tcp(options) => options.socket = None,
            Self::Udp(options) => options.socket = None,
        }
        tracing::trace!("SocketSender::disconnect() end");
    }

    pub async fn send(&mut self, data: &[u8]) {
        tracing::trace!("SocketSender::send() start");
        loop {
            self.connect().await;
            let result = match self {
                Self::Tcp(options) => {
                    let socket = options.socket.as_mut().expect("Connected prior");
                    match socket.write_all(data).await {
                        Ok(()) => socket.flush().await,
                        Err(err) => Err(err),
                    }
                }
                Self::Udp(options) => {
                    let data = if data.len() > MAX_UDP_PACKET_SIZE {
                        // Strip to max UDP packet size
                        &data[..MAX_UDP_PACKET_SIZE]
                    } else {
                        // Just strip newline
                        &data[..data.len() - 1]
                    };

                    options
                        .socket
                        .as_ref()
                        .expect("Connected prior")
                        .send(data)
                        .await
                        .map(|_| ())
                }
            };
            match result {
                Ok(()) => break,
                Err(err) => {
                    tracing::warn!("Error sending data: {err}");
                    self.disconnect();
                    tokio::time::sleep(ONE_SECOND).await;
                    continue;
                }
            }
        }
        tracing::trace!("SocketSender::send() end");
    }
    pub async fn clear_receive(&mut self) {
        let mut empty_buf = [0u8; 512];
        match self {
            Self::Tcp(SocketOptions {
                socket: Some(socket),
                ..
            }) => socket.read(&mut empty_buf).await,
            Self::Udp(SocketOptions {
                socket: Some(socket),
                ..
            }) => socket.recv(&mut empty_buf).await,
            _ => pending().await,
        }
        .unwrap_or(0);
    }

    pub const fn protocol(&self) -> &'static str {
        match self {
            Self::Tcp(_) => "tcp",
            Self::Udp(_) => "udp",
        }
    }

    pub const fn socket_addr(&self) -> &SocketAddr {
        match self {
            Self::Tcp(options) => &options.addr,
            Self::Udp(options) => &options.addr,
        }
    }

    pub fn url(&self) -> String {
        match self {
            Self::Tcp(options) => format!("tcp://{}", options.addr),
            Self::Udp(options) => format!("udp://{}", options.addr),
        }
    }
}

async fn socket(
    mut socket_sender: SocketSender,
    cancellation_token: CancellationToken,
    mut rx: Receiver<EmitterData>,
) {
    tracing::trace!(
        "socket(protocol = \"{}\", addr = \"{}\") start",
        socket_sender.protocol(),
        socket_sender.socket_addr()
    );
    tracing::info!("Sending logs to {}", socket_sender.url());
    loop {
        tokio::select! {
            biased;
            () = cancellation_token.cancelled() => break,
            value = rx.recv() => {
                match value {
                    None => break,
                    Some(data) => tokio::select! {
                        biased;
                        () = socket_sender.send(&data) => {},
                        () = cancellation_token.cancelled() => break,
                    },
                }
            },
            () = socket_sender.clear_receive() => {},
        }
    }
    tracing::trace!(
        "socket(protocol = \"{}\", addr = \"{}\") end",
        socket_sender.protocol(),
        socket_sender.socket_addr()
    );
}

struct MaybeSignal {
    signal: Option<tokio::signal::unix::Signal>,
}

impl MaybeSignal {
    pub fn new(signal_kind: SignalKind) -> Self {
        let signal = tokio::signal::unix::signal(signal_kind).ok();
        Self { signal }
    }

    pub async fn recv(&mut self) {
        match self.signal.as_mut() {
            Some(signal) => match signal.recv().await {
                Some(()) => (),
                None => self.signal = None,
            },
            None => pending().await,
        }
    }
}

async fn file_append(path: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .await
}

async fn file(path: PathBuf, cancellation_token: CancellationToken, mut rx: Receiver<EmitterData>) {
    tracing::trace!("file(path = \"{}\") start", path.display());
    let mut file = match file_append(&path).await {
        Ok(file) => file,
        Err(err) => {
            log::error!("Could not open emitter file: {err}");
            cancellation_token.cancel();
            return;
        }
    };

    let mut signal = MaybeSignal::new(SignalKind::hangup());

    tracing::info!("Saving logs to file \"{}\"", path.display());
    tracing::trace!("file(path = \"{}\") loop", path.display());
    loop {
        tokio::select! {
            biased;
            () = cancellation_token.cancelled() => break,
            () = signal.recv() => {
                match file_append(&path).await {
                    Ok(new_file) => {
                        file = new_file;
                        tracing::info!("Switched to new emitter file");
                    },
                    Err(err) => {
                        tracing::warn!("Could not switch to new emitter file: {err}");
                    }
                }
            },
            value = rx.recv() => {
                match value {
                    None => break,
                    Some(data) => {
                        if let Err(err) = file.write_all(&data).await {
                            log::warn!("Could not write to emitter file: {err}");
                        }
                        if let Err(err) = file.flush().await {
                            log::warn!("Could not flush emitter file: {err}");
                        }
                    },
                }
            }
        }
    }
    tracing::trace!("file(path = \"{}\") end", path.display());
}

pub fn start(
    url: String,
    cancellation_token: CancellationToken,
    rx: Receiver<EmitterData>,
) -> Result<Pin<Box<dyn Future<Output = ()> + Send>>, String> {
    match url {
        url if url.starts_with("tcp://") => match url[6..].parse() {
            Ok(socket_addr) => Ok(Box::pin(socket(
                SocketSender::tcp(socket_addr),
                cancellation_token,
                rx,
            ))),
            Err(err) => Err(format!("Error parsing url: {err}")),
        },
        url if url.starts_with("tcp:") => match url[4..].parse() {
            Ok(socket_addr) => Ok(Box::pin(socket(
                SocketSender::tcp(socket_addr),
                cancellation_token,
                rx,
            ))),
            Err(err) => Err(format!("Error parsing url: {err}")),
        },
        url if url.starts_with("udp://") => match url[6..].parse() {
            Ok(socket_addr) => Ok(Box::pin(socket(
                SocketSender::udp(socket_addr),
                cancellation_token,
                rx,
            ))),
            Err(err) => Err(format!("Error parsing url: {err}")),
        },
        url if url.starts_with("udp:") => match url[4..].parse() {
            Ok(socket_addr) => Ok(Box::pin(socket(
                SocketSender::udp(socket_addr),
                cancellation_token,
                rx,
            ))),
            Err(err) => Err(format!("Error parsing url: {err}")),
        },
        url if url.starts_with("file://") => {
            let path = PathBuf::from(&url[7..]);
            Ok(Box::pin(file(path, cancellation_token, rx)))
        }
        url if url.starts_with("file:") => {
            let path = PathBuf::from(&url[5..]);
            let Some(parent) = path.parent() else {
                return Err("Given file path is root".to_string());
            };
            if !parent.is_dir() {
                return Err(format!(
                    "Parent path is not a directory: {}",
                    parent.display()
                ));
            }

            Ok(Box::pin(file(path, cancellation_token, rx)))
        }
        _ => Err("Unknown url type".to_string()),
    }
}
