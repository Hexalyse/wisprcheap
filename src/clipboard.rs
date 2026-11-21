//! Text clipboard on a dedicated thread (on X11 the clipboard content is served by the process that owns
//! it, so one long-lived `arboard::Clipboard` is kept for the whole session).

use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::sync::oneshot;

const RETRIES: usize = 5;
const RETRY_DELAY: Duration = Duration::from_millis(15);

enum Request {
    Read(oneshot::Sender<Result<String>>),
    Write(String, oneshot::Sender<Result<()>>),
}

fn read_with_retry(clipboard: &mut arboard::Clipboard) -> Result<String> {
    let mut last_err = None;
    for attempt in 0..RETRIES {
        match clipboard.get_text() {
            Ok(text) => return Ok(text),
            // Non-text content (an image, files...) reads as empty text.
            Err(arboard::Error::ContentNotAvailable) => return Ok(String::new()),
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < RETRIES {
                    std::thread::sleep(RETRY_DELAY);
                }
            }
        }
    }
    Err(anyhow!("{}", last_err.unwrap()))
}

fn write_with_retry(clipboard: &mut arboard::Clipboard, text: &str) -> Result<()> {
    let mut last_err = None;
    for attempt in 0..RETRIES {
        match clipboard.set_text(text.to_owned()) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < RETRIES {
                    std::thread::sleep(RETRY_DELAY);
                }
            }
        }
    }
    Err(anyhow!("{}", last_err.unwrap()))
}

fn sender() -> &'static mpsc::Sender<Request> {
    static TX: OnceLock<mpsc::Sender<Request>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Request>();
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                let mut clipboard: Option<arboard::Clipboard> = None;
                for request in rx {
                    if clipboard.is_none() {
                        clipboard = arboard::Clipboard::new().ok();
                    }
                    match request {
                        Request::Read(reply) => {
                            let result = match clipboard.as_mut() {
                                None => Err(anyhow!("the clipboard is unavailable")),
                                Some(c) => read_with_retry(c),
                            };
                            let _ = reply.send(result);
                        }
                        Request::Write(text, reply) => {
                            let result = match clipboard.as_mut() {
                                None => Err(anyhow!("the clipboard is unavailable")),
                                Some(c) => write_with_retry(c, &text),
                            };
                            let _ = reply.send(result);
                        }
                    }
                }
            })
            .expect("clipboard thread");
        tx
    })
}

pub async fn read() -> Result<String> {
    let (tx, rx) = oneshot::channel();
    sender()
        .send(Request::Read(tx))
        .map_err(|_| anyhow!("the clipboard thread stopped"))?;
    rx.await
        .map_err(|_| anyhow!("the clipboard thread stopped"))?
}

pub async fn write(text: impl Into<String>) -> Result<()> {
    let (tx, rx) = oneshot::channel();
    sender()
        .send(Request::Write(text.into(), tx))
        .map_err(|_| anyhow!("the clipboard thread stopped"))?;
    rx.await
        .map_err(|_| anyhow!("the clipboard thread stopped"))?
}
