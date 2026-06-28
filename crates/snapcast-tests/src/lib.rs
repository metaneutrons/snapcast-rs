//! Integration test helpers.

use snapcast_client::{ClientConfig, ClientEvent, SnapClient};
use snapcast_server::{ServerConfig, ServerEvent, SnapServer};
use tokio::sync::mpsc;

/// Bind an ephemeral 127.0.0.1 port, spawn `server.serve()` on it, and return
/// the actual bound port. The library opens no port itself, so tests bind here;
/// reading the bound port avoids a bind/connect race.
pub async fn spawn_serving(mut server: SnapServer) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        server.serve(listener).await.ok();
    });
    // Give the accept loop a moment to start.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    port
}

/// Server handle with event receiver and audio sender.
pub struct TestServer {
    pub events: mpsc::Receiver<ServerEvent>,
    pub audio_tx: mpsc::Sender<snapcast_server::AudioFrame>,
    pub cmd: mpsc::Sender<snapcast_server::ServerCommand>,
    pub port: u16,
}

/// Start a default server on a random port. Returns the handle once serving.
pub async fn start_server() -> TestServer {
    let (mut server, events) = SnapServer::new(ServerConfig::default());
    let audio_tx = server.add_stream("default");
    let cmd = server.command_sender();
    let port = spawn_serving(server).await;
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
        host: "127.0.0.1".into(),
        port,
        ..ClientConfig::default()
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
            Ok(None) => panic!("Event channel closed"),
            _ => panic!("Timed out waiting for expected event"),
        }
    }
}
