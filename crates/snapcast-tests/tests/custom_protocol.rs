#![cfg(feature = "custom-protocol")]

use snapcast_client::ClientEvent;
use snapcast_proto::CustomMessage;
use snapcast_server::ServerEvent;
use snapcast_tests::{connect_client, expect_event, start_server};

#[tokio::test]
async fn server_sends_custom_message_to_client() {
    let mut server = start_server().await;
    let mut client = connect_client(server.port).await;

    // Wait for connection
    expect_event(&mut client.events, 2000, |e| {
        matches!(e, ClientEvent::StreamStarted { .. }).then_some(())
    })
    .await;

    // Get client ID from server
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    let client_id = loop {
        match tokio::time::timeout_at(deadline, server.events.recv()).await {
            Ok(Some(ServerEvent::ClientConnected { id, .. })) => break id,
            Ok(Some(_)) => continue,
            _ => panic!("Timed out waiting for ClientConnected"),
        }
    };

    // Server sends custom message
    server
        .cmd
        .send(snapcast_server::ServerCommand::SendToClient {
            client_id,
            message: CustomMessage::new(9, b"hello from server"),
        })
        .await
        .unwrap();

    // Client receives it
    let msg = expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::CustomMessage(msg) => Some(msg),
        _ => None,
    })
    .await;

    assert_eq!(msg.type_id, 9);
    assert_eq!(msg.payload, b"hello from server");
}
