//! In-session recurring prompt scheduling for `/loop`.

use super::*;

pub(super) const DEFAULT_LOOP_INTERVAL: Duration = Duration::from_secs(270);
const MIN_LOOP_INTERVAL: Duration = Duration::from_secs(10);
const MAX_LOOP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const BUSY_RETRY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LoopCommand {
    Start { interval: Duration, prompt: String },
    Status,
    Stop,
}

pub(super) struct LoopSchedule {
    pub(super) generation: u64,
    pub(super) thread_id: ThreadId,
    pub(super) interval: Duration,
    pub(super) prompt: String,
    pub(super) wake_task: Option<tokio::task::JoinHandle<()>>,
}

impl LoopSchedule {
    fn cancel_wake(&mut self) {
        if let Some(task) = self.wake_task.take() {
            task.abort();
        }
    }
}

pub(super) fn parse_loop_command(args: &str) -> Result<LoopCommand, String> {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("status") {
        return Ok(LoopCommand::Status);
    }
    if trimmed.eq_ignore_ascii_case("stop") {
        return Ok(LoopCommand::Stop);
    }

    let (first, rest) = trimmed
        .split_once(char::is_whitespace)
        .map(|(first, rest)| (first, rest.trim()))
        .unwrap_or((trimmed, ""));
    if let Some(interval) = parse_loop_interval(first)? {
        if rest.is_empty() {
            return Err("A recurring prompt is required after the interval.".to_string());
        }
        return Ok(LoopCommand::Start {
            interval,
            prompt: rest.to_string(),
        });
    }

    Ok(LoopCommand::Start {
        interval: DEFAULT_LOOP_INTERVAL,
        prompt: trimmed.to_string(),
    })
}

fn parse_loop_interval(token: &str) -> Result<Option<Duration>, String> {
    let Some((digits, unit)) = token
        .find(|character: char| !character.is_ascii_digit())
        .map(|index| token.split_at(index))
    else {
        return Ok(None);
    };
    if digits.is_empty() || !matches!(unit, "s" | "m" | "h") {
        return Ok(None);
    }
    let amount = digits
        .parse::<u64>()
        .map_err(|_| format!("Invalid loop interval: {token}"))?;
    let seconds = match unit {
        "s" => amount,
        "m" => amount.saturating_mul(60),
        "h" => amount.saturating_mul(60 * 60),
        _ => unreachable!("unit was validated above"),
    };
    let interval = Duration::from_secs(seconds);
    if !(MIN_LOOP_INTERVAL..=MAX_LOOP_INTERVAL).contains(&interval) {
        return Err(format!(
            "Loop interval must be between {} and {}.",
            format_loop_interval(MIN_LOOP_INTERVAL),
            format_loop_interval(MAX_LOOP_INTERVAL)
        ));
    }
    Ok(Some(interval))
}

pub(super) fn format_loop_interval(interval: Duration) -> String {
    let seconds = interval.as_secs();
    if seconds % 3_600 == 0 {
        format!("{}h", seconds / 3_600)
    } else if seconds % 60 == 0 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

impl ChatWidget {
    pub(super) fn handle_loop_command(&mut self, args: &str) {
        match parse_loop_command(args) {
            Ok(LoopCommand::Status) => self.show_loop_status(),
            Ok(LoopCommand::Stop) => self.stop_loop_schedule(/*show_status*/ true),
            Ok(LoopCommand::Start { interval, prompt }) => {
                self.start_loop_schedule(interval, prompt)
            }
            Err(error) => self.add_error_message(format!(
                "{error} Usage: /loop [10s|5m|1h] <prompt>; /loop status; /loop stop"
            )),
        }
    }

    fn start_loop_schedule(&mut self, interval: Duration, prompt: String) {
        let Some(thread_id) = self.thread_id else {
            self.add_error_message("'/loop' is unavailable before the session starts.".to_string());
            return;
        };
        if self.blocks_direct_input {
            self.add_error_message(PARENT_OWNED_INPUT_MESSAGE.to_string());
            return;
        }

        self.stop_loop_schedule(/*show_status*/ false);
        self.loop_generation = self.loop_generation.wrapping_add(1);
        self.loop_schedule = Some(LoopSchedule {
            generation: self.loop_generation,
            thread_id,
            interval,
            prompt,
            wake_task: None,
        });
        self.schedule_next_loop_wake(interval);
        self.show_loop_status();
    }

    fn show_loop_status(&mut self) {
        let Some(schedule) = self.loop_schedule.as_ref() else {
            self.add_info_message(
                "No recurring loop is active.".to_string(),
                Some("Start one with /loop [10s|5m|1h] <prompt>.".to_string()),
            );
            return;
        };
        let interval = format_loop_interval(schedule.interval);
        let prompt = schedule.prompt.clone();
        self.add_info_message(
            format!("Loop active every {interval}: {prompt}"),
            Some("Ticks wait while a turn is active. Stop with /loop stop.".to_string()),
        );
    }

    pub(super) fn stop_loop_schedule(&mut self, show_status: bool) {
        let stopped = self.loop_schedule.take().map(|mut schedule| {
            schedule.cancel_wake();
        });
        self.loop_generation = self.loop_generation.wrapping_add(1);
        if show_status {
            let message = if stopped.is_some() {
                "Recurring loop stopped."
            } else {
                "No recurring loop is active."
            };
            self.add_info_message(message.to_string(), /*hint*/ None);
        }
    }

    fn schedule_next_loop_wake(&mut self, delay: Duration) {
        let Some(schedule) = self.loop_schedule.as_mut() else {
            return;
        };
        schedule.cancel_wake();
        let app_event_tx = self.app_event_tx.clone();
        let thread_id = schedule.thread_id;
        let generation = schedule.generation;
        schedule.wake_task = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            app_event_tx.send(AppEvent::LoopTick {
                thread_id,
                generation,
            });
        }));
    }

    pub(crate) fn on_loop_tick(&mut self, thread_id: ThreadId, generation: u64) {
        let Some(schedule) = self.loop_schedule.as_mut() else {
            return;
        };
        if schedule.thread_id != thread_id || schedule.generation != generation {
            return;
        }
        schedule.wake_task.take();

        if self.thread_id != Some(thread_id) {
            self.stop_loop_schedule(/*show_status*/ false);
            return;
        }
        if self.is_user_turn_pending_or_running() || !self.bottom_pane.no_modal_or_popup_active() {
            self.schedule_next_loop_wake(BUSY_RETRY_INTERVAL);
            return;
        }

        let Some(schedule) = self.loop_schedule.as_ref() else {
            return;
        };
        let prompt = schedule.prompt.clone();
        let interval = schedule.interval;
        // A loop is a recurring model prompt, never an implicit local shell
        // scheduler. Preserve a leading `!` as ordinary user text.
        self.submit_user_message_as_plain_user_turn(UserMessage::from(prompt));
        self.schedule_next_loop_wake(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatwidget::tests::Op;
    use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
    use tokio::sync::mpsc::error::TryRecvError;

    #[test]
    fn parses_default_cache_aware_loop() {
        assert_eq!(
            parse_loop_command("audit the current work").unwrap(),
            LoopCommand::Start {
                interval: DEFAULT_LOOP_INTERVAL,
                prompt: "audit the current work".to_string(),
            }
        );
    }

    #[test]
    fn parses_explicit_interval_and_controls() {
        assert_eq!(
            parse_loop_command("20m role call every bound profile").unwrap(),
            LoopCommand::Start {
                interval: Duration::from_secs(1_200),
                prompt: "role call every bound profile".to_string(),
            }
        );
        assert_eq!(parse_loop_command("status").unwrap(), LoopCommand::Status);
        assert_eq!(parse_loop_command("stop").unwrap(), LoopCommand::Stop);
    }

    #[test]
    fn rejects_unsafe_or_incomplete_intervals() {
        assert!(parse_loop_command("1s too fast").is_err());
        assert!(parse_loop_command("5m").is_err());
        assert!(parse_loop_command("25h too long").is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn loop_emits_one_tick_and_stop_cancels_the_successor() {
        let (mut chat, _sender, mut app_rx, _op_rx) = make_chatwidget_manual_with_sender().await;
        let thread_id = ThreadId::new();
        chat.thread_id = Some(thread_id);

        chat.handle_loop_command("10s inspect the current state");
        let generation = chat.loop_schedule.as_ref().expect("active loop").generation;

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        let tick = std::iter::from_fn(|| app_rx.try_recv().ok()).find(|event| {
            matches!(
                event,
                AppEvent::LoopTick {
                    thread_id: event_thread_id,
                    generation: event_generation,
                } if *event_thread_id == thread_id && *event_generation == generation
            )
        });
        assert!(tick.is_some(), "expected exactly one loop tick");
        assert!(matches!(app_rx.try_recv(), Err(TryRecvError::Empty)));

        chat.on_loop_tick(thread_id, generation);
        assert!(chat.loop_schedule.is_some());
        chat.handle_loop_command("stop");
        assert!(chat.loop_schedule.is_none());

        tokio::time::advance(Duration::from_secs(20)).await;
        tokio::task::yield_now().await;
        assert!(
            std::iter::from_fn(|| app_rx.try_recv().ok())
                .all(|event| !matches!(event, AppEvent::LoopTick { .. })),
            "stopped loop emitted another tick"
        );
    }

    #[tokio::test]
    async fn loop_preserves_leading_bang_as_model_input() {
        let (mut chat, _sender, _app_rx, mut op_rx) = make_chatwidget_manual_with_sender().await;
        let thread_id = ThreadId::new();
        chat.thread_id = Some(thread_id);

        chat.handle_loop_command("10s !echo must-not-run");
        let generation = chat.loop_schedule.as_ref().expect("active loop").generation;
        chat.on_loop_tick(thread_id, generation);

        match op_rx.try_recv().expect("loop submission") {
            Op::UserTurn { items, .. } => assert_eq!(
                items,
                vec![UserInput::Text {
                    text: "!echo must-not-run".to_string(),
                    text_elements: Vec::new(),
                }]
            ),
            Op::RunUserShellCommand { command } => {
                panic!("loop executed a local shell command: {command}")
            }
            other => panic!("expected loop model turn, got {other:?}"),
        }

        chat.handle_loop_command("stop");
    }

    #[tokio::test(start_paused = true)]
    async fn loop_defers_while_a_turn_is_pending_without_overlapping() {
        let (mut chat, _sender, mut app_rx, mut op_rx) = make_chatwidget_manual_with_sender().await;
        let thread_id = ThreadId::new();
        chat.thread_id = Some(thread_id);
        chat.handle_loop_command("10s inspect after the active turn");
        let generation = chat.loop_schedule.as_ref().unwrap().generation;

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        let first_tick = std::iter::from_fn(|| app_rx.try_recv().ok())
            .find(|event| matches!(event, AppEvent::LoopTick { .. }))
            .expect("initial loop tick");
        chat.input_queue.user_turn_pending_start = true;
        if let AppEvent::LoopTick {
            thread_id,
            generation,
        } = first_tick
        {
            chat.on_loop_tick(thread_id, generation);
        }
        assert!(matches!(op_rx.try_recv(), Err(TryRecvError::Empty)));

        chat.input_queue.user_turn_pending_start = false;
        tokio::task::yield_now().await;
        tokio::time::advance(BUSY_RETRY_INTERVAL).await;
        tokio::task::yield_now().await;
        let retry_tick = std::iter::from_fn(|| app_rx.try_recv().ok())
            .find(|event| {
                matches!(
                    event,
                    AppEvent::LoopTick {
                        thread_id: event_thread_id,
                        generation: event_generation,
                    } if *event_thread_id == thread_id && *event_generation == generation
                )
            })
            .expect("busy retry tick");
        if let AppEvent::LoopTick {
            thread_id,
            generation,
        } = retry_tick
        {
            chat.on_loop_tick(thread_id, generation);
        }
        assert!(matches!(op_rx.try_recv(), Ok(Op::UserTurn { .. })));
        chat.handle_loop_command("stop");
    }

    #[tokio::test(start_paused = true)]
    async fn loop_tick_for_a_replaced_thread_cancels_without_submission() {
        let (mut chat, _sender, mut app_rx, mut op_rx) = make_chatwidget_manual_with_sender().await;
        let original_thread_id = ThreadId::new();
        chat.thread_id = Some(original_thread_id);
        chat.handle_loop_command("10s inspect the original thread");

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        let tick = std::iter::from_fn(|| app_rx.try_recv().ok())
            .find(|event| matches!(event, AppEvent::LoopTick { .. }))
            .expect("loop tick");
        chat.thread_id = Some(ThreadId::new());
        if let AppEvent::LoopTick {
            thread_id,
            generation,
        } = tick
        {
            chat.on_loop_tick(thread_id, generation);
        }

        assert!(chat.loop_schedule.is_none());
        assert!(matches!(op_rx.try_recv(), Err(TryRecvError::Empty)));
    }
}
