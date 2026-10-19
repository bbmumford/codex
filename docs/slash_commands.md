# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

## Recurring in-session work

`/loop [interval] <prompt>` runs a prompt repeatedly in the current session. The
interval accepts seconds, minutes, or hours (`30s`, `5m`, `1h`) and defaults to
270 seconds so prompt-cache context stays warm. Intervals must be between 10
seconds and 24 hours.

Only one loop is active per session. A wake that arrives while a turn or modal
interaction is active waits and retries; it never steers or overlaps the active
turn. Changing threads or ending the session cancels the loop.

- `/loop status` shows the active schedule.
- `/loop stop` cancels it.
