use std::{
    net::SocketAddr,
    ops::ControlFlow,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{
        ConnectInfo, Path, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use serde::Deserialize;
use tokio::sync::broadcast::{self, error::RecvError};

use crate::{
    error::ApiError,
    security::{
        USER_ID_LEN, client_ip, ip_key, is_safe_id, is_valid_room_id, now, origin_allowed,
        random_id,
    },
    state::{AppState, Room, RoomEvent},
};

/// Max messages a client may send per second before being disconnected.
const MAX_MESSAGES_PER_SEC: u32 = 60;
const CLOSE_SEALED: u16 = 4001;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Content {
        content: String,
    },
    P2pSignal {
        to: String,
        signal: serde_json::Value,
    },
    P2pRequest {
        file_id: String,
    },
}

/// One WebSocket slot of a client IP, released on drop.
pub struct WsSlot {
    map: Arc<DashMap<String, usize>>,
    key: String,
}

impl WsSlot {
    fn acquire(map: &Arc<DashMap<String, usize>>, key: String, max: usize) -> Option<Self> {
        let mut count = map.entry(key.clone()).or_insert(0);
        if *count >= max {
            return None;
        }
        *count += 1;
        drop(count);
        Some(Self {
            map: map.clone(),
            key,
        })
    }
}

impl Drop for WsSlot {
    fn drop(&mut self) {
        if let Some(mut count) = self.map.get_mut(&self.key) {
            *count = count.saturating_sub(1);
        }
        self.map.remove_if(&self.key, |_, count| *count == 0);
    }
}

pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    Path(room_id): Path<String>,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !is_valid_room_id(&room_id) {
        return ApiError::not_found("Room introuvable").into_response();
    }
    if !origin_allowed(&headers, state.config.trust_proxy) {
        return ApiError::new(StatusCode::FORBIDDEN, "Origine refusée").into_response();
    }
    let ip = ip_key(client_ip(&headers, peer, &state.config));
    if !state.limiter.hit(format!("ws:{ip}"), 30, 60) {
        return ApiError::too_many().into_response();
    }

    // Protected rooms are only readable through /unlock.
    match state.db.get_room(&room_id).await {
        Ok(None) => {}
        Ok(Some(_)) => {
            return ApiError::new(StatusCode::FORBIDDEN, "Room protégée").into_response();
        }
        Err(e) => return ApiError::from(e).into_response(),
    }

    if !state.rooms.contains_key(&room_id) && state.rooms.len() >= state.config.max_rooms {
        return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "Serveur saturé").into_response();
    }

    let Some(slot) = WsSlot::acquire(&state.ws_per_ip, ip, state.config.max_ws_per_ip) else {
        return ApiError::too_many().into_response();
    };

    // JSON escaping can double the size of the text.
    let max_message = state.config.max_content_size * 2 + 64 * 1024;
    ws.max_message_size(max_message)
        .max_frame_size(max_message)
        .on_upgrade(move |socket| handle_socket(socket, room_id, state, slot))
}

/// One connected client of a room.
#[derive(Clone)]
struct Session {
    state: AppState,
    room_id: String,
    user_id: String,
    tx: broadcast::Sender<RoomEvent>,
}

impl Session {
    /// Joins (or creates) the room; returns the session, its event receiver,
    /// the current text and the number of users.
    fn join(
        state: AppState,
        room_id: String,
    ) -> (Self, broadcast::Receiver<RoomEvent>, String, usize) {
        let user_id = random_id(USER_ID_LEN);
        let (tx, rx, content, users) = {
            let mut room = state.rooms.entry(room_id.clone()).or_insert_with(Room::new);
            room.users.insert(user_id.clone());
            room.empty_since = None;
            (
                room.tx.clone(),
                room.tx.subscribe(),
                room.content.clone(),
                room.users.len(),
            )
        };
        let _ = tx.send(RoomEvent::Users { count: users });
        let session = Self {
            state,
            room_id,
            user_id,
            tx,
        };
        (session, rx, content, users)
    }

    /// The room (text and files) is kept for a grace period after the last
    /// user leaves, so a page reload does not wipe it; see maintenance.rs.
    fn leave(&self) {
        let remaining = self.state.rooms.get_mut(&self.room_id).map(|mut room| {
            room.users.remove(&self.user_id);
            if room.users.is_empty() {
                room.empty_since = Some(now());
            }
            room.users.len()
        });
        if let Some(count) = remaining {
            let _ = self.tx.send(RoomEvent::Users { count });
        }
    }

    /// Whether this client should receive `event`.
    fn wants(&self, event: &RoomEvent) -> bool {
        match event {
            RoomEvent::Content { from, .. } | RoomEvent::P2PRequest { from, .. } => {
                from != &self.user_id
            }
            RoomEvent::P2PSignal { to, .. } => to == &self.user_id,
            _ => true,
        }
    }

    /// Room events -> client.
    async fn forward_events(
        self,
        mut sender: SplitSink<WebSocket, Message>,
        mut rx: broadcast::Receiver<RoomEvent>,
    ) {
        loop {
            let event = match rx.recv().await {
                Ok(event) => event,
                // Too slow to keep up: resynchronise with the current text.
                Err(RecvError::Lagged(_)) => {
                    let Some(content) = self
                        .state
                        .rooms
                        .get(&self.room_id)
                        .map(|r| r.content.clone())
                    else {
                        break;
                    };
                    RoomEvent::Content {
                        content,
                        from: "server".to_string(),
                    }
                }
                Err(RecvError::Closed) => break,
            };
            if !self.wants(&event) {
                continue;
            }
            let Ok(msg) = serde_json::to_string(&event) else {
                continue;
            };
            if sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
            if matches!(event, RoomEvent::Sealed) {
                let close = CloseFrame {
                    code: CLOSE_SEALED,
                    reason: "sealed".into(),
                };
                let _ = sender.send(Message::Close(Some(close))).await;
                break;
            }
        }
    }

    /// Client messages -> room.
    async fn handle_incoming(self, mut receiver: SplitStream<WebSocket>) {
        let mut window = Instant::now();
        let mut count = 0u32;

        while let Some(Ok(msg)) = receiver.next().await {
            if window.elapsed() >= Duration::from_secs(1) {
                window = Instant::now();
                count = 0;
            }
            count += 1;
            if count > MAX_MESSAGES_PER_SEC {
                tracing::warn!("closing flooding websocket in room {}", self.room_id);
                break;
            }

            let text = match msg {
                Message::Text(text) => text,
                Message::Close(_) => break,
                _ => continue,
            };
            if let Ok(message) = serde_json::from_str::<ClientMessage>(text.as_str())
                && self.handle_message(message).is_break()
            {
                break;
            }
        }
    }

    fn handle_message(&self, message: ClientMessage) -> ControlFlow<()> {
        match message {
            ClientMessage::Content { content } => {
                if content.len() > self.state.config.max_content_size {
                    return ControlFlow::Continue(());
                }
                match self.state.rooms.get_mut(&self.room_id) {
                    Some(mut room) => room.content.clone_from(&content),
                    // The room has been protected meanwhile.
                    None => return ControlFlow::Break(()),
                }
                let _ = self.tx.send(RoomEvent::Content {
                    content,
                    from: self.user_id.clone(),
                });
            }
            ClientMessage::P2pSignal { to, signal } => {
                let is_member = self
                    .state
                    .rooms
                    .get(&self.room_id)
                    .is_some_and(|r| r.users.contains(&to));
                if is_member {
                    let _ = self.tx.send(RoomEvent::P2PSignal {
                        from: self.user_id.clone(),
                        to,
                        signal,
                    });
                }
            }
            ClientMessage::P2pRequest { file_id } => {
                if is_safe_id(&file_id) {
                    let _ = self.tx.send(RoomEvent::P2PRequest {
                        from: self.user_id.clone(),
                        file_id,
                    });
                }
            }
        }
        ControlFlow::Continue(())
    }
}

async fn handle_socket(socket: WebSocket, room_id: String, state: AppState, _slot: WsSlot) {
    let (session, rx, content, users) = Session::join(state, room_id);
    let (mut sender, receiver) = socket.split();

    let init = serde_json::json!({
        "type": "init",
        "content": content,
        "userId": session.user_id,
        "users": users
    });
    let _ = sender.send(Message::Text(init.to_string().into())).await;

    let mut send_task = tokio::spawn(session.clone().forward_events(sender, rx));
    let mut recv_task = tokio::spawn(session.clone().handle_incoming(receiver));
    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
    session.leave();
}
