use axum::extract::ws::{Message, WebSocket};
use futures::{sink::SinkExt, stream::StreamExt};
use std::sync::Arc;
use sqlx::SqlitePool;
use serde::{Deserialize, Serialize};
use crate::AppState;

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum WsClientMessage {
    UpdateContent {
        content: String,
    },
    DeleteRoom
}


#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum WsServerMessage {
    UpdatedContent {
        content: String,
    },
    RoomDeleted
}

pub async fn handle_socket(mut socket: WebSocket, room: String, state: Arc<AppState>) {
    let room_clone = room.clone();

    let (mut sender, mut receiver) = socket.split();


    let tx = state.get_or_create_room_channel(room).await;
    let mut rx = tx.subscribe();

    // Handle incoming messages
    let mut send_task = tokio::spawn(async move {
        while let Ok(content) = rx.recv().await {
            let message = serde_json::to_string(&content).unwrap();
            if sender.send(Message::Text(message)).await.is_err() {
                break;
            }
        }
    });

    // Handle outgoing messages
    let db = state.db.clone();
    
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(text))) = receiver.next().await {
            info!("Received message: {text}");
            if let Ok(message) = serde_json::from_str::<WsClientMessage>(&text) {
                info!("Message: {:?}", message);
                match message {
                    WsClientMessage::UpdateContent { content } => {
                        // Save to database first
                        match sqlx::query!(
                            "INSERT OR REPLACE INTO rooms (id, content) VALUES (?, ?)",
                            room_clone,
                            content
                        )
                        .execute(&db)
                        .await {
                            Ok(_) => {
                                // Only broadcast if database update was successful
                                let _ = tx.send(WsServerMessage::UpdatedContent {
                                    content,
                                });
                            }
                            Err(e) => {
                                eprintln!("Database error: {:?}", e);
                            }
                        }
                    },
                    WsClientMessage::DeleteRoom => {
                        // Save to database first
                        match sqlx::query!(
                            "DELETE FROM rooms WHERE id = ?",
                            room_clone,
                        )
                        .execute(&db)
                        .await {
                            Ok(_) => {
                                // Only broadcast if database update was successful
                                let _ = tx.send(WsServerMessage::RoomDeleted);
                            }
                            Err(e) => {
                                eprintln!("Database error: {:?}", e);
                            }
                        }
                    },
                }
            }
        }
    });

    // Wait for either task to finish
    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
}
