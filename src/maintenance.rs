//! Background cleanup: idle rooms, expired protected rooms, orphan files.

use crate::{
    error::ApiResult,
    files::{purge_orphan_files, purge_room_files},
    security::now,
    state::AppState,
};

pub async fn startup_cleanup(state: &AppState) -> ApiResult<()> {
    let temp = state.db.delete_temp_rooms().await?;
    if temp > 0 {
        tracing::info!("removed {temp} temporary protected room(s)");
    }
    sweep_database(state).await?;

    // Files on disk that the database does not know about.
    let known = state.db.stored_file_ids().await?;
    for id in state.storage.list_ids().await? {
        if !known.contains(&id) {
            state.storage.remove(&id).await;
        }
    }
    Ok(())
}

pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(state.config.sweep_interval);
    interval.tick().await;
    loop {
        interval.tick().await;
        sweep_idle_rooms(&state);
        if let Err(e) = sweep_database(&state).await {
            tracing::warn!("cleanup failed: {}", e.message);
        }
        let now = now();
        state.file_tokens.retain(|_, t| t.expires_at > now);
        state.limiter.purge();
    }
}

fn sweep_idle_rooms(state: &AppState) {
    let deadline = now() - state.config.room_idle_grace;
    let is_idle =
        |r: &crate::state::Room| r.users.is_empty() && r.empty_since.is_some_and(|t| t <= deadline);

    let idle: Vec<String> = state
        .rooms
        .iter()
        .filter(|r| is_idle(r.value()))
        .map(|r| r.key().clone())
        .collect();
    for id in idle {
        if state.rooms.remove_if(&id, |_, r| is_idle(r)).is_some() {
            tracing::info!("cleaned up idle room {id}");
        }
    }
}

async fn sweep_database(state: &AppState) -> ApiResult<()> {
    for id in state.db.expired_room_ids(now()).await? {
        purge_room_files(state, &id).await?;
        state.db.delete_room(&id).await?;
        tracing::info!("deleted expired room {id}");
    }
    let orphans = purge_orphan_files(state).await?;
    if orphans > 0 {
        tracing::info!("deleted {orphans} orphan file(s)");
    }
    Ok(())
}
