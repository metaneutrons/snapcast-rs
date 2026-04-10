//! Integration test helpers.

use std::net::TcpListener;

use snapcast_client::config::{PlayerSettings, ServerSettings};
use snapcast_client::{ClientConfig, ClientEvent, SnapClient};
use snapcast_server::{ServerConfig, ServerEvent, SnapServer};
use tokio::sync::mpsc;

/// Find a free TCP port on localhost.
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Server handle with event receiver and audio sender.
pub struct TestServer {
    pub events: mpsc::Receiver<ServerEvent>,
    pub audio_tx: mpsc::Sender<snapcast_server::AudioFrame>,
    pub cmd: mpsc::Sender<snapcast_server::ServerCommand>,
    pub port: u16,
}

/// Start a server on a random port. Returns handle after server is listening.
pub async fn start_server() -> TestServer {
    let port = free_port();
    let config = ServerConfig {
        stream_port: port,
        ..ServerConfig::default()
    };
    let (mut server, events, audio_tx) = SnapServer::new(config);
    let cmd = server.command_sender();
    tokio::spawn(async move {
        server.run().await.ok();
    });
    // Give the listener time to bind
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    TestServer {
        events,
        audio_tx,
        cmd,
        port,
    }
}

/// Client handle with event receiver.
pub struct TestClient {
    pub events: mpsc::Receiver<ClientEvent>,
    pub audio_rx: mpsc::Receiver<snapcast_client::AudioFrame>,
    pub cmd: mpsc::Sender<snapcast_client::ClientCommand>,
}

/// Connect a client to the given server port.
pub async fn connect_client(port: u16) -> TestClient {
    let config = ClientConfig {
        server: ServerSettings {
            scheme: "tcp".into(),
            host: "127.0.0.1".into(),
            port,
            ..ServerSettings::default()
        },
        player: PlayerSettings::default(),
        host_id: String::new(),
        instance: 1,
    };
    let (mut client, events, audio_rx) = SnapClient::new(config);
    let cmd = client.command_sender();
    tokio::spawn(async move {
        client.run().await.ok();
    });
    TestClient {
        events,
        audio_rx,
        cmd,
    }
}

/// Wait for a specific event, with timeout.
pub async fn expect_event<F, T>(
    events: &mut mpsc::Receiver<ClientEvent>,
    timeout_ms: u64,
    mut f: F,
) -> T
where
    F: FnMut(ClientEvent) -> Option<T>,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(event)) => {
                if let Some(val) = f(event) {
                    return val;
                }
            }
            _ => panic!("Timed out waiting for expected event"),
        }
    }
}
