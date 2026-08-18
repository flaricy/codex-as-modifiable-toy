# Changelog

## 0.2.6 - 2026-08-18

### Added

- Added automatic resolution for `request_user_input` prompts after 60 seconds.
- Added the optional `autoResolutionMs` tool argument to customize the timeout
  from 60 to 240 seconds. Values outside that range are clamped.

### Behavior

- On timeout, Codex selects the option whose label ends with `(Recommended)`.
- If no option is marked as recommended, Codex selects the first option.
- Keyboard or paste interaction continues to snooze automatic resolution for
  the active prompt.

Upstream Codex changes are documented on the
[OpenAI Codex releases page](https://github.com/openai/codex/releases).
