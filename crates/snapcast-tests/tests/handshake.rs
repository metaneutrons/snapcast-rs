use snapcast_client::ClientEvent;
use snapcast_tests::{connect_client, expect_event, start_server};

#[tokio::test]
async fn client_connects_and_receives_settings() {
    let server = start_server().await;
    let mut client = connect_client(server.port).await;

    // Client should receive Connected + ServerSettings
    expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::Connected { .. } => Some(()),
        _ => None,
    })
    .await;

    let (buffer_ms, volume) = expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::ServerSettings {
            buffer_ms, volume, ..
        } => Some((buffer_ms, volume)),
        _ => None,
    })
    .await;

    assert!(buffer_ms > 0);
    assert!(volume > 0);
}

#[tokio::test]
async fn client_receives_codec_header() {
    let server = start_server().await;
    let mut client = connect_client(server.port).await;

    expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::StreamStarted { codec, format } => {
            assert!(!codec.is_empty());
            assert!(format.rate() > 0);
            Some(())
        }
        _ => None,
    })
    .await;
}
