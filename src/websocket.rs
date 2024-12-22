use axum::extract::ws::{Message, WebSocket};
use futures::{sink::SinkExt, stream::StreamExt};
use std::sync::Arc;
use sqlx::SqlitePool;
use crate::AppState;

pub async fn handle_socket(mut socket: WebSocket, room: String, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.tx.subscribe();

    // Handle incoming messages
    let mut send_task = tokio::spawn(async move {
        while let Ok(content) = rx.recv().await {
            // Send the message to the WebSocket client
            if sender.send(Message::Text(content)).await.is_err() {
                break;
            }
        }
    });

    // Handle outgoing messages
    let tx = state.tx.clone();
    let db = state.db.clone();
    let room_clone = room.clone();
    
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(text))) = receiver.next().await {
            // Save to database first
            match sqlx::query!(
                "INSERT OR REPLACE INTO rooms (id, content) VALUES (?, ?)",
                room_clone,
                text
            )
            .execute(&db)
            .await {
                Ok(_) => {
                    // Only broadcast if database update was successful
                    let _ = tx.send(text);
                }
                Err(e) => {
                    eprintln!("Database error: {:?}", e);
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
