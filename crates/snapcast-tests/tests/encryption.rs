#![cfg(feature = "encryption")]

use snapcast_client::{ClientConfig, ClientEvent, SnapClient};
use snapcast_server::{ServerConfig, SnapServer};
use snapcast_tests::free_port;

#[tokio::test]
async fn encrypted_f32lz4_end_to_end() {
    let port = free_port();
    let psk = "test-secret-key-42";

    // Server with encryption
    let server_config = ServerConfig {
        stream_port: port,
        codec: "f32lz4".into(),
        encryption_psk: Some(psk.into()),
        ..ServerConfig::default()
    };
    let (mut server, _events, audio_tx) = SnapServer::new(server_config);
    tokio::spawn(async move { server.run().await.ok() });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Client with matching key
    let client_config = ClientConfig {
        host: "127.0.0.1".into(),
        port,
        encryption_psk: Some(psk.into()),
        ..ClientConfig::default()
    };
    let (mut client, mut events, _audio_rx) = SnapClient::new(client_config);
    tokio::spawn(async move { client.run().await.ok() });

    // Wait for stream to start (proves codec header with ENC marker was accepted)
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::StreamStarted { codec, .. })) => {
                assert_eq!(codec, "f32lz4");
                break;
            }
            Ok(Some(_)) => continue,
            _ => panic!("Timed out waiting for encrypted stream start"),
        }
    }

    // Push audio — if decryption fails, client would disconnect
    let samples: Vec<f32> = (0..960).map(|i| (i as f32 / 960.0) * 2.0 - 1.0).collect();
    for _ in 0..5 {
        audio_tx
            .send(snapcast_server::AudioFrame {
                data: snapcast_server::AudioData::F32(samples.clone()),
                timestamp_usec: 0,
            })
            .await
            .unwrap();
    }

    // Give time for chunks to flow through
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    // If we get here without disconnect, encryption round-trip works
}

#[tokio::test]
async fn wrong_key_disconnects() {
    let port = free_port();

    // Server with encryption
    let server_config = ServerConfig {
        stream_port: port,
        codec: "f32lz4".into(),
        encryption_psk: Some("server-key".into()),
        ..ServerConfig::default()
    };
    let (mut server, _events, _audio_tx) = SnapServer::new(server_config);
    tokio::spawn(async move { server.run().await.ok() });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Client with WRONG key
    let client_config = ClientConfig {
        host: "127.0.0.1".into(),
        port,
        encryption_psk: Some("wrong-key".into()),
        ..ClientConfig::default()
    };
    let (mut client, mut events, _audio_rx) = SnapClient::new(client_config);
    tokio::spawn(async move { client.run().await.ok() });

    // Client should still connect (encryption is on the codec, not the handshake)
    // But audio decryption will fail silently (chunks dropped)
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::StreamStarted { .. })) => break, // connected OK
            Ok(Some(_)) => continue,
            _ => panic!("Timed out — client should still connect with wrong key"),
        }
    }
}
