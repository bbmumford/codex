use codex_login::AuthManager;
use std::sync::Arc;

/// Installs the local AccountDock credential-reload signal listener.
///
/// SIGUSR1 is deliberately handled only by this custom fork. Reloading updates
/// the shared auth revision; account-scoped model transports then discard any
/// WebSocket and turn state belonging to the previous account before the next
/// request.
#[cfg(unix)]
pub(crate) fn install(auth_manager: Arc<AuthManager>) {
    tokio::spawn(async move {
        let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
        else {
            tracing::warn!("failed to install AccountDock auth reload signal listener");
            return;
        };
        while signal.recv().await.is_some() {
            let changed = auth_manager.reload().await;
            tracing::info!(changed, "reloaded credentials after AccountDock signal");
        }
    });
}

#[cfg(not(unix))]
pub(crate) fn install(_auth_manager: Arc<AuthManager>) {}
