//! Command dispatch — handles each [`ServerCommand`] against shared state.
//!
//! Extracted from the `run()` loop so the state machine is one focused unit and
//! the crate root keeps to API types plus the server facade. `Stop`/`None`
//! terminate the loop and are handled by `run()` itself; every other command
//! lands here.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use crate::session::SessionServer;
use crate::state::ServerState;
use crate::{ClientSettingsUpdate, ServerCommand, ServerEvent};

/// The handles and config a command handler needs (everything but loop control).
pub(crate) struct Dispatcher {
    /// Shared server state, guarded for concurrent access with the session task.
    pub(crate) shared_state: Arc<Mutex<ServerState>>,
    /// Session server — pushes settings and recomputes audio routing.
    pub(crate) session_srv: Arc<SessionServer>,
    /// Outbound event channel to the embedder.
    pub(crate) event_tx: mpsc::Sender<ServerEvent>,
    /// Optional state-persistence file.
    pub(crate) state_file: Option<PathBuf>,
    /// Playout buffer size in milliseconds (pushed to clients).
    pub(crate) buffer_ms: i32,
}

impl Dispatcher {
    /// Persist state to the configured file, logging (not propagating) errors.
    fn save_state(&self, s: &ServerState) {
        if let Some(ref path) = self.state_file {
            let _ = s
                .save(path)
                .map_err(|e| tracing::warn!(error = %e, "Failed to save state"));
        }
    }

    /// Handle one command.
    ///
    /// `Stop`/`None` are handled by the run loop and never reach here.
    pub(crate) async fn dispatch(&self, cmd: ServerCommand) {
        match cmd {
            ServerCommand::Stop => unreachable!("Stop is handled by the run loop"),
            ServerCommand::SetClientVolume {
                client_id,
                volume,
                muted,
            } => {
                let mut s = self.shared_state.lock().await;
                if let Some(c) = s.clients.get_mut(&client_id) {
                    c.config.volume.percent = volume;
                    c.config.volume.muted = muted;
                }
                let latency = s
                    .clients
                    .get(&client_id)
                    .map(|c| c.config.latency)
                    .unwrap_or(0);
                self.save_state(&s);
                drop(s);
                self.session_srv
                    .push_settings(ClientSettingsUpdate {
                        client_id: client_id.clone(),
                        buffer_ms: self.buffer_ms,
                        latency,
                        volume,
                        muted,
                    })
                    .await;
                let _ = self.event_tx.try_send(ServerEvent::ClientVolumeChanged {
                    client_id: client_id.clone(),
                    volume,
                    muted,
                });
                self.session_srv.update_routing_for_client(&client_id).await;
            }
            ServerCommand::SetClientLatency { client_id, latency } => {
                let mut settings_update = None;
                let mut s = self.shared_state.lock().await;
                if let Some(c) = s.clients.get_mut(&client_id) {
                    c.config.latency = latency;
                    settings_update = Some(ClientSettingsUpdate {
                        client_id: client_id.clone(),
                        buffer_ms: self.buffer_ms,
                        latency,
                        volume: c.config.volume.percent,
                        muted: c.config.volume.muted,
                    });
                }
                self.save_state(&s);
                drop(s);
                if let Some(update) = settings_update {
                    self.session_srv.push_settings(update).await;
                }
                let _ = self
                    .event_tx
                    .try_send(ServerEvent::ClientLatencyChanged { client_id, latency });
            }
            ServerCommand::SetClientName { client_id, name } => {
                let mut s = self.shared_state.lock().await;
                if let Some(c) = s.clients.get_mut(&client_id) {
                    c.config.name = name.clone();
                }
                self.save_state(&s);
                drop(s);
                let _ = self
                    .event_tx
                    .try_send(ServerEvent::ClientNameChanged { client_id, name });
            }
            ServerCommand::SetGroupStream {
                group_id,
                stream_id,
            } => {
                let mut s = self.shared_state.lock().await;
                s.set_group_stream(&group_id, &stream_id);
                self.save_state(&s);
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::GroupStreamChanged {
                    group_id: group_id.clone(),
                    stream_id,
                });
                self.session_srv.update_routing_for_group(&group_id).await;
            }
            ServerCommand::SetGroupMute { group_id, muted } => {
                let mut s = self.shared_state.lock().await;
                if let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) {
                    g.muted = muted;
                }
                self.save_state(&s);
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::GroupMuteChanged {
                    group_id: group_id.clone(),
                    muted,
                });
                self.session_srv.update_routing_for_group(&group_id).await;
            }
            ServerCommand::SetGroupName { group_id, name } => {
                let mut s = self.shared_state.lock().await;
                if let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) {
                    g.name = name.clone();
                }
                self.save_state(&s);
                drop(s);
                let _ = self
                    .event_tx
                    .try_send(ServerEvent::GroupNameChanged { group_id, name });
            }
            ServerCommand::SetGroupClients { group_id, clients } => {
                let mut s = self.shared_state.lock().await;
                s.set_group_clients(&group_id, &clients);
                self.save_state(&s);
                drop(s);
                // Structural change — mirrors Server.OnUpdate in C++ snapserver
                let _ = self.event_tx.try_send(ServerEvent::ServerUpdated);
                self.session_srv.update_routing_all().await;
            }
            ServerCommand::DeleteClient { client_id } => {
                let mut s = self.shared_state.lock().await;
                s.remove_client_from_groups(&client_id);
                s.clients.remove(&client_id);
                self.save_state(&s);
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::ServerUpdated);
                self.session_srv.update_routing_all().await;
            }
            ServerCommand::SetStreamMeta {
                stream_id,
                metadata,
            } => {
                let mut s = self.shared_state.lock().await;
                if let Some(stream) = s.streams.iter_mut().find(|st| st.id == stream_id) {
                    stream.properties = metadata.clone();
                }
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::StreamMetaChanged {
                    stream_id,
                    metadata,
                });
            }
            ServerCommand::AddStream { uri, response_tx } => {
                tracing::warn!(
                    uri,
                    "Dynamic stream addition requires application-owned stream orchestration"
                );
                let _ = response_tx.send(Err(
                    "dynamic Stream.AddStream is not supported by the embeddable server after run(); create streams before run()".into(),
                ));
            }
            ServerCommand::RemoveStream { stream_id } => {
                let mut s = self.shared_state.lock().await;
                s.streams.retain(|st| st.id != stream_id);
                // Clear stream_id on groups that referenced this stream
                for g in &mut s.groups {
                    if g.stream_id == stream_id {
                        g.stream_id.clear();
                    }
                }
                self.save_state(&s);
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::ServerUpdated);
                self.session_srv.update_routing_all().await;
            }
            ServerCommand::StreamControl {
                stream_id,
                command,
                params,
            } => {
                tracing::debug!(stream_id, command, ?params, "Stream control forwarded");
                // Forward to embedder via event — the library doesn't own stream readers
                let _ = self.event_tx.try_send(ServerEvent::StreamControl {
                    stream_id,
                    command,
                    params,
                });
            }
            ServerCommand::GetStatus { response_tx } => {
                let s = self.shared_state.lock().await;
                let _ = response_tx.send(s.to_status());
            }
            #[cfg(feature = "custom-protocol")]
            ServerCommand::SendToClient { client_id, message } => {
                self.session_srv
                    .send_custom(&client_id, message.type_id, message.payload)
                    .await;
            }
        }
    }
}
