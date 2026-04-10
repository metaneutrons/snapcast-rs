use snapcast_client::{ClientConfig, ClientEvent, SnapClient};
use snapcast_server::auth::{Role, StaticAuthValidator, User};
use snapcast_server::{ServerConfig, SnapServer};
use snapcast_tests::free_port;
use std::sync::Arc;

fn auth_server_config(port: u16) -> ServerConfig {
    ServerConfig {
        stream_port: port,
        auth: Some(Arc::new(StaticAuthValidator::new(
            vec![User {
                name: "player".into(),
                password: "secret".into(),
                role: "streaming".into(),
            }],
            vec![Role {
                name: "streaming".into(),
                permissions: vec!["Streaming".into()],
            }],
        ))),
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn auth_success() {
    let port = free_port();
    let (mut server, _events) = SnapServer::new(auth_server_config(port));
    let _audio_tx = server.add_stream("default");
    tokio::spawn(async move { server.run().await.ok() });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    use base64::Engine;
    let creds = base64::engine::general_purpose::STANDARD.encode("player:secret");

    let config = ClientConfig {
        host: "127.0.0.1".into(),
        port,
        auth: Some(snapcast_client::config::Auth {
            scheme: "Basic".into(),
            param: creds,
        }),
        ..ClientConfig::default()
    };
    let (mut client, mut events, _audio_rx) = SnapClient::new(config);
    tokio::spawn(async move { client.run().await.ok() });

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::ServerSettings { .. })) => break, // success
            Ok(Some(_)) => continue,
            _ => panic!("Auth should have succeeded — timed out"),
        }
    }
}

#[tokio::test]
async fn auth_rejection() {
    let port = free_port();
    let (mut server, _events) = SnapServer::new(auth_server_config(port));
    let _audio_tx = server.add_stream("default");
    tokio::spawn(async move { server.run().await.ok() });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    use base64::Engine;
    let creds = base64::engine::general_purpose::STANDARD.encode("player:wrong");

    let config = ClientConfig {
        host: "127.0.0.1".into(),
        port,
        auth: Some(snapcast_client::config::Auth {
            scheme: "Basic".into(),
            param: creds,
        }),
        ..ClientConfig::default()
    };
    let (mut client, mut events, _audio_rx) = SnapClient::new(config);
    tokio::spawn(async move { client.run().await.ok() });

    // Client should get Disconnected (server sends Error and drops connection)
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::Disconnected { reason })) => {
                assert!(
                    reason.contains("401")
                        || reason.contains("Unauthorized")
                        || reason.contains("rejected"),
                    "Expected auth rejection, got: {reason}"
                );
                break;
            }
            Ok(Some(ClientEvent::ServerSettings { .. })) => {
                panic!("Should not have received ServerSettings with wrong password");
            }
            Ok(Some(_)) => continue,
            _ => panic!("Timed out waiting for auth rejection"),
        }
    }
}
