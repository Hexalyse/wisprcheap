//! Single instance over a per-user local socket (a named pipe on Windows, a Unix socket on Linux).
//! The running app listens on it, and `wisprcheap start` / `stop` / the desktop shortcut talk to it
//! ("ping", "quit", "show-log"...). WISPRCHEAP_INSTANCE gives tests their own socket.

use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::paths::{instance_suffix, user_name};

/// A command received by the running instance, with the channel for its one-line reply.
pub type Request = (String, oneshot::Sender<String>);

#[derive(Debug)]
pub enum AcquireError {
    AlreadyRunning,
    Other(std::io::Error),
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcquireError::AlreadyRunning => f.write_str("wisprcheap is already running."),
            AcquireError::Other(e) => write!(f, "could not open the control socket: {e}"),
        }
    }
}

fn suffix() -> String {
    instance_suffix()
        .map(|s| format!("-{s}"))
        .unwrap_or_default()
}

#[cfg(windows)]
pub fn socket_name() -> String {
    format!(r"\\.\pipe\wisprcheap-{}{}", user_name(), suffix())
}

#[cfg(unix)]
pub fn socket_name() -> String {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("wisprcheap-{}{}.sock", user_name(), suffix()))
        .to_string_lossy()
        .into_owned()
}

async fn exchange<S>(stream: S, command: &str, timeout: Duration) -> String
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let work = async move {
        let mut stream = stream;
        stream
            .write_all(format!("{command}\n").as_bytes())
            .await
            .ok()?;
        let mut data = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            let n = stream.read(&mut buf).await.ok()?;
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            if data.contains(&b'\n') {
                break;
            }
        }
        Some(String::from_utf8_lossy(&data).trim().to_string())
    };
    tokio::time::timeout(timeout, work)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Send one command. Returns the reply, or None when no instance is running.
pub async fn send_command(command: &str, timeout: Duration) -> Option<String> {
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        let deadline = Instant::now() + timeout;
        let client = loop {
            match ClientOptions::new().open(socket_name()) {
                Ok(c) => break c,
                // All pipe instances busy: the server is creating the next one.
                Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(_) => return None,
            }
        };
        Some(exchange(client, command, timeout).await)
    }
    #[cfg(unix)]
    {
        let _ = Instant::now();
        let stream = tokio::net::UnixStream::connect(socket_name()).await.ok()?;
        Some(exchange(stream, command, timeout).await)
    }
}

async fn serve_connection<S>(stream: S, requests: mpsc::UnboundedSender<Request>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await;
    if !matches!(read, Ok(Ok(n)) if n > 0) {
        return;
    }
    let (tx, rx) = oneshot::channel();
    if requests.send((line.trim().to_string(), tx)).is_err() {
        return;
    }
    let reply = rx.await.unwrap_or_default();
    let mut stream = reader.into_inner();
    let _ = stream.write_all(format!("{reply}\n").as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The listening side; dropping it stops listening.
pub struct Instance {
    task: tokio::task::JoinHandle<()>,
    #[cfg(unix)]
    path: String,
}

impl Instance {
    pub fn close(&self) {
        self.task.abort();
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(windows)]
fn listen_once(requests: &mpsc::UnboundedSender<Request>) -> Result<Instance, AcquireError> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = socket_name();
    let first = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&name)
        .map_err(|e| {
            // ERROR_ACCESS_DENIED / ERROR_PIPE_BUSY: another instance owns the pipe.
            if matches!(e.raw_os_error(), Some(5) | Some(231)) {
                AcquireError::AlreadyRunning
            } else {
                AcquireError::Other(e)
            }
        })?;
    let requests = requests.clone();
    let task = tokio::spawn(async move {
        let mut server = first;
        loop {
            let connected = server.connect().await;
            // Create the next instance before releasing this one, so the name stays taken.
            let next = match ServerOptions::new().create(&name) {
                Ok(s) => s,
                Err(e) => {
                    crate::warn!("[instance] control pipe failed: {e}");
                    return;
                }
            };
            let current = std::mem::replace(&mut server, next);
            if connected.is_ok() {
                tokio::spawn(serve_connection(current, requests.clone()));
            }
        }
    });
    Ok(Instance { task })
}

#[cfg(unix)]
fn listen_once(requests: &mpsc::UnboundedSender<Request>) -> Result<Instance, AcquireError> {
    let path = socket_name();
    // A live instance answers on the socket; a stale file from a crash doesn't.
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        return Err(AcquireError::AlreadyRunning);
    }
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path).map_err(AcquireError::Other)?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let requests = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(serve_connection(stream, requests.clone()));
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    });
    Ok(Instance { task, path })
}

/// Become the running instance. Waits up to `wait` for a previous instance to exit (restart).
pub async fn acquire_instance(
    requests: mpsc::UnboundedSender<Request>,
    wait: Duration,
) -> Result<Instance, AcquireError> {
    let deadline = Instant::now() + wait;
    loop {
        match listen_once(&requests) {
            Ok(instance) => return Ok(instance),
            Err(AcquireError::AlreadyRunning) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(e) => return Err(e),
        }
    }
}
