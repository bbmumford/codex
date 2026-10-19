use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;

use codex_hooks::BackgroundWatcher;
use codex_utils_path_uri::PathUri;
use tokio::io::AsyncReadExt;
use tokio::time::Instant;
use tokio::time::Sleep;
use tokio_util::sync::CancellationToken;

use super::HookWatcherEntry;
use super::UnifiedExecProcessManager;
use super::async_watcher::BACKGROUND_OUTPUT_WAKE_MAX_BYTES;
use super::async_watcher::BACKGROUND_OUTPUT_WAKE_RETRY_INTERVAL;
use super::async_watcher::background_output_wake_sleep;
use super::async_watcher::flush_background_output_wake;
use super::head_tail_buffer::HeadTailBuffer;
use crate::session::session::Session;

static NEXT_HOOK_WATCHER_PROCESS_ID: AtomicI32 = AtomicI32::new(-1);

impl UnifiedExecProcessManager {
    pub(crate) fn apply_hook_background_watchers(
        &self,
        session: Arc<Session>,
        watchers: Vec<BackgroundWatcher>,
    ) {
        let mut requested = HashMap::new();
        for watcher in watchers {
            requested.insert(watcher.id.clone(), watcher);
        }

        for (id, directive) in requested {
            if !valid_directive(&directive) {
                tracing::warn!(watcher_id = %id, "ignoring invalid hook background watcher");
                continue;
            }

            let mut watchers = self
                .hook_watchers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if watchers
                .get(&id)
                .is_some_and(|entry| entry.directive == directive && !entry.task.is_finished())
            {
                continue;
            }
            if let Some(previous) = watchers.remove(&id) {
                previous.cancellation.cancel();
                previous.task.abort();
            }

            let cancellation = CancellationToken::new();
            let watcher_session = Arc::clone(&session);
            let watcher_directive = directive.clone();
            let watcher_cancellation = cancellation.clone();
            let task = tokio::spawn(async move {
                run_hook_watcher(watcher_session, watcher_directive, watcher_cancellation).await;
            });
            watchers.insert(
                id,
                HookWatcherEntry {
                    directive,
                    cancellation,
                    task,
                },
            );
        }
    }

    pub(super) fn terminate_hook_watchers(&self) {
        let entries = {
            let mut watchers = self
                .hook_watchers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            watchers.drain().map(|(_, entry)| entry).collect::<Vec<_>>()
        };
        for entry in entries {
            entry.cancellation.cancel();
            entry.task.abort();
        }
    }
}

fn valid_directive(directive: &BackgroundWatcher) -> bool {
    let Some(executable) = directive.command.first() else {
        return false;
    };
    if !PathBuf::from(executable).is_absolute() {
        return false;
    }
    directive
        .cwd
        .as_ref()
        .is_none_or(|cwd| PathBuf::from(cwd).is_absolute())
}

async fn run_hook_watcher(
    session: Arc<Session>,
    directive: BackgroundWatcher,
    cancellation: CancellationToken,
) {
    let process_id = NEXT_HOOK_WATCHER_PROCESS_ID.fetch_sub(1, Ordering::Relaxed);
    let command_label = directive.command.join(" ");
    let cwd_path = directive
        .cwd
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let Some(cwd) = PathUri::from_host_native_path(&cwd_path).ok() else {
        tracing::warn!(
            watcher_id = %directive.id,
            cwd = %cwd_path.display(),
            "hook background watcher has an invalid cwd"
        );
        return;
    };

    let mut command = tokio::process::Command::new(&directive.command[0]);
    command
        .args(&directive.command[1..])
        .current_dir(&cwd_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(
                watcher_id = %directive.id,
                error = %error,
                "failed to start hook background watcher"
            );
            return;
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill().await;
        return;
    };

    let mut bytes = vec![0_u8; 8192];
    let mut pending = HeadTailBuffer::new(BACKGROUND_OUTPUT_WAKE_MAX_BYTES);
    let mut flush_sleep: Option<Pin<Box<Sleep>>> = None;
    let mut last_delivery = None;
    let mut local_claims = std::collections::VecDeque::new();

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                break;
            }
            _ = async {
                if let Some(sleep) = flush_sleep.as_mut() {
                    sleep.as_mut().await;
                }
            }, if flush_sleep.is_some() => {
                let delivered = flush_background_output_wake(
                    &mut pending,
                    &session,
                    process_id,
                    &command_label,
                    &cwd,
                    &mut local_claims,
                ).await;
                if delivered {
                    last_delivery = Some(Instant::now());
                    flush_sleep = None;
                } else if pending.total_bytes() > 0 {
                    flush_sleep = Some(Box::pin(tokio::time::sleep(
                        BACKGROUND_OUTPUT_WAKE_RETRY_INTERVAL,
                    )));
                } else {
                    flush_sleep = None;
                }
            }
            read = stdout.read(&mut bytes) => {
                match read {
                    Ok(0) => {
                        let _ = flush_background_output_wake(
                            &mut pending,
                            &session,
                            process_id,
                            &command_label,
                            &cwd,
                            &mut local_claims,
                        ).await;
                        let _ = child.wait().await;
                        break;
                    }
                    Ok(count) => {
                        pending.push_chunk(bytes[..count].to_vec());
                        if flush_sleep.is_none() {
                            flush_sleep = Some(background_output_wake_sleep(last_delivery));
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            watcher_id = %directive.id,
                            error = %error,
                            "hook background watcher stdout failed"
                        );
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        break;
                    }
                }
            }
        }
    }
}
