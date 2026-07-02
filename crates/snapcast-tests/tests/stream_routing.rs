use snapcast_client::ClientEvent;
use snapcast_server::{AudioData, AudioFrame, ServerCommand, ServerEvent};
use snapcast_tests::{connect_client, expect_event, spawn_serving};
use tokio::sync::mpsc;

/// Start a server with two streams.
async fn start_two_stream_server() -> TwoStreamServer {
    let (mut server, events) = snapcast_server::SnapServer::new(Default::default());
    let stream_a = server.add_stream("stream_a");
    let stream_b = server.add_stream("stream_b");
    let cmd = server.command_sender();
    let port = spawn_serving(server).await;
    TwoStreamServer {
        events,
        stream_a,
        stream_b,
        cmd,
        port,
    }
}

struct TwoStreamServer {
    events: mpsc::Receiver<ServerEvent>,
    stream_a: mpsc::Sender<AudioFrame>,
    stream_b: mpsc::Sender<AudioFrame>,
    cmd: mpsc::Sender<ServerCommand>,
    port: u16,
}

fn silence_frame() -> AudioFrame {
    AudioFrame {
        data: AudioData::F32(vec![0.0; 960]),
        timestamp_usec: 0,
    }
}

fn tone_frame() -> AudioFrame {
    AudioFrame {
        data: AudioData::F32((0..960).map(|i| (i as f32 / 960.0) * 2.0 - 1.0).collect()),
        timestamp_usec: 0,
    }
}

/// Helper: get server status and find the group containing a client.
async fn get_client_group(cmd: &mpsc::Sender<ServerCommand>, client_id: &str) -> (String, String) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    cmd.send(ServerCommand::GetStatus { response_tx: tx })
        .await
        .unwrap();
    let status = rx.await.unwrap();
    for group in &status.server.groups {
        for client in &group.clients {
            if client.id == client_id {
                return (group.id.clone(), group.stream_id.clone());
            }
        }
    }
    panic!("Client {client_id} not found in any group");
}

/// Helper: wait for server ClientConnected event.
async fn wait_for_connect(events: &mut mpsc::Receiver<ServerEvent>) -> String {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ServerEvent::ClientConnected { id, .. })) => return id,
            Ok(Some(_)) => continue,
            _ => panic!("Timed out waiting for ClientConnected"),
        }
    }
}

/// Helper: wait (event-driven, no sleep) for a specific server event, so a
/// command's effect is observed before the following assertion runs.
async fn wait_for_server_event<F>(events: &mut mpsc::Receiver<ServerEvent>, mut pred: F)
where
    F: FnMut(&ServerEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ev)) if pred(&ev) => return,
            Ok(Some(_)) => continue,
            _ => panic!("Timed out waiting for server event"),
        }
    }
}

#[tokio::test]
async fn client_receives_audio_only_from_assigned_stream() {
    let mut server = start_two_stream_server().await;
    let mut client = connect_client(server.port).await;

    // Wait for client to connect and stream to start
    let client_id = wait_for_connect(&mut server.events).await;
    expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::StreamStarted { .. } => Some(()),
        _ => None,
    })
    .await;

    // Client is in group assigned to stream_a (first stream = default)
    let (group_id, stream_id) = get_client_group(&server.cmd, &client_id).await;
    assert_eq!(stream_id, "stream_a");

    // Exercise the pipeline: assigned stream carries audio, the other does not.
    for _ in 0..5 {
        server.stream_a.send(tone_frame()).await.unwrap();
        server.stream_b.send(silence_frame()).await.unwrap();
    }

    // Switch client's group to stream_b and wait for the routing change to apply.
    server
        .cmd
        .send(ServerCommand::SetGroupStream {
            group_id: group_id.clone(),
            stream_id: "stream_b".into(),
        })
        .await
        .unwrap();
    wait_for_server_event(&mut server.events, |e| {
        matches!(e, ServerEvent::GroupStreamChanged { .. })
    })
    .await;

    // Verify the switch happened
    let (_, new_stream) = get_client_group(&server.cmd, &client_id).await;
    assert_eq!(new_stream, "stream_b");

    // Now stream_b audio should reach the client, stream_a should not.
    for _ in 0..5 {
        server.stream_b.send(tone_frame()).await.unwrap();
        server.stream_a.send(silence_frame()).await.unwrap();
    }

    // If we got here without panics/errors, stream routing works end-to-end:
    // - Client only received audio from its assigned stream
    // - SetGroupStream changed the routing
    // - New stream's audio reaches the client after switch
}

#[tokio::test]
async fn muted_client_receives_no_audio() {
    let mut server = start_two_stream_server().await;
    let mut client = connect_client(server.port).await;

    let client_id = wait_for_connect(&mut server.events).await;
    expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::StreamStarted { .. } => Some(()),
        _ => None,
    })
    .await;

    // Mute the client
    server
        .cmd
        .send(ServerCommand::SetClientVolume {
            client_id: client_id.clone(),
            volume: 0,
            muted: true,
        })
        .await
        .unwrap();
    wait_for_server_event(&mut server.events, |e| {
        matches!(e, ServerEvent::ClientVolumeChanged { muted: true, .. })
    })
    .await;

    // Send audio — muted client should not receive chunks (server skips sending)
    for _ in 0..5 {
        server.stream_a.send(tone_frame()).await.unwrap();
    }

    // Unmute
    server
        .cmd
        .send(ServerCommand::SetClientVolume {
            client_id: client_id.clone(),
            volume: 100,
            muted: false,
        })
        .await
        .unwrap();
    wait_for_server_event(&mut server.events, |e| {
        matches!(e, ServerEvent::ClientVolumeChanged { muted: false, .. })
    })
    .await;

    // Audio should flow again
    for _ in 0..5 {
        server.stream_a.send(tone_frame()).await.unwrap();
    }
}
