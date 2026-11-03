use eyre::{Result, bail};
use serde_json::Value;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify};
use tracing::{error, info, warn};

const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

#[derive(Default)]
struct MessageQueue {
    messages: Mutex<VecDeque<Vec<u8>>>,
    available: Notify,
}

impl MessageQueue {
    async fn push(&self, message: Vec<u8>) {
        self.messages.lock().await.push_back(message);
        self.available.notify_one();
    }

    async fn pop(&self) -> Vec<u8> {
        loop {
            let available = self.available.notified();
            if let Some(message) = self.messages.lock().await.pop_front() {
                return message;
            }
            available.await;
        }
    }
}

pub(crate) struct WcpRelay {
    address: SocketAddr,
    client_to_server: MessageQueue,
    server_to_client: MessageQueue,
    served_file: Option<PathBuf>,
    viewer_url: Mutex<Option<String>>,
    rewritten_load: Mutex<Option<(String, String)>>,
}

impl WcpRelay {
    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }

    pub(crate) async fn next_client_message(&self) -> Vec<u8> {
        let message = self.client_to_server.pop().await;
        info!(
            "WCP client -> server: {}",
            String::from_utf8_lossy(&message)
        );
        message
    }

    pub(crate) async fn send_server_message(&self, message: Vec<u8>) -> Result<()> {
        let mut decoded = serde_json::from_slice::<Value>(&message)?;
        let mut rewritten = false;
        if decoded.get("type").and_then(Value::as_str) == Some("event")
            && decoded.get("event").and_then(Value::as_str) == Some("waveforms_loaded")
            && let Some((url, source)) = self.rewritten_load.lock().await.take()
            && decoded.get("source").and_then(Value::as_str) == Some(url.as_str())
        {
            decoded["source"] = Value::String(source);
            rewritten = true;
        }
        let message = if rewritten {
            serde_json::to_vec(&decoded)?
        } else {
            message
        };
        info!(
            "WCP server -> client: {}",
            String::from_utf8_lossy(&message)
        );
        self.server_to_client.push(message).await;
        Ok(())
    }

    pub(crate) async fn set_viewer_url(&self, url: String) -> Result<()> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            bail!("Invalid Surver viewer URL");
        }
        *self.viewer_url.lock().await = Some(url);
        Ok(())
    }

    async fn send_client_message(&self, message: Vec<u8>) -> Result<()> {
        let mut decoded = serde_json::from_slice::<Value>(&message)?;
        let mut rewritten = false;
        if decoded.get("type").and_then(Value::as_str) == Some("command")
            && decoded.get("command").and_then(Value::as_str) == Some("load")
            && let Some(source) = decoded
                .get("source")
                .and_then(Value::as_str)
                .map(str::to_string)
            && self.served_file.as_ref().is_some_and(|served_file| {
                Path::new(&source)
                    .canonicalize()
                    .is_ok_and(|source| source == *served_file)
            })
            && let Some(url) = self.viewer_url.lock().await.clone()
        {
            decoded["source"] = Value::String(url.clone());
            *self.rewritten_load.lock().await = Some((url, source));
            rewritten = true;
        }
        self.client_to_server
            .push(if rewritten {
                serde_json::to_vec(&decoded)?
            } else {
                message
            })
            .await;
        Ok(())
    }

    async fn clear(&self) {
        self.client_to_server.messages.lock().await.clear();
        self.server_to_client.messages.lock().await.clear();
    }
}

pub(crate) async fn start(address: String, filenames: &[String]) -> Result<Arc<WcpRelay>> {
    let listener = TcpListener::bind(&address).await?;
    let address = listener.local_addr()?;
    let relay = Arc::new(WcpRelay {
        address,
        client_to_server: MessageQueue::default(),
        server_to_client: MessageQueue::default(),
        served_file: (filenames.len() == 1)
            .then(|| Path::new(&filenames[0]).canonicalize().ok())
            .flatten(),
        viewer_url: Mutex::default(),
        rewritten_load: Mutex::default(),
    });
    let relay_copy = relay.clone();
    info!("WCP relay listening on {address}");

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    info!("WCP client connected from {peer}");
                    if let Err(error) = handle_client(stream, &relay_copy).await {
                        warn!("WCP client disconnected with error: {error:#}");
                    } else {
                        info!("WCP client disconnected");
                    }
                    relay_copy.clear().await;
                }
                Err(error) => error!("Failed to accept WCP connection: {error:#}"),
            }
        }
    });

    Ok(relay)
}

async fn handle_client(stream: TcpStream, relay: &WcpRelay) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let mut pending = Vec::new();
    let mut read_buffer = [0_u8; 8192];

    loop {
        tokio::select! {
            count = reader.read(&mut read_buffer) => {
                let count = count?;
                if count == 0 {
                    return Ok(());
                }

                pending.extend_from_slice(&read_buffer[..count]);
                while let Some(frame_end) = pending.iter().position(|byte| *byte == 0) {
                    let frame = pending.drain(..frame_end).collect::<Vec<_>>();
                    pending.remove(0);
                    let message = serde_json::from_slice::<Value>(&frame)?;
                    if message.get("type").and_then(Value::as_str) == Some("command")
                        && message.get("command").and_then(Value::as_str) == Some("shutdown")
                    {
                        return Ok(());
                    }
                    relay.send_client_message(frame).await?;
                }

                if pending.len() > MAX_FRAME_LEN {
                    bail!("WCP frame exceeds the {MAX_FRAME_LEN}-byte limit");
                }
            }
            message = relay.server_to_client.pop() => {
                writer.write_all(&message).await?;
                writer.write_all(&[0]).await?;
                writer.flush().await?;
            }
        }
    }
}
