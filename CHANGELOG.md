# Changelog

## Unreleased

### Upstream compatibility

- Rebased the Autowake distribution on upstream Codex CLI 0.148.0.
- Preserved ChatGPT account authentication and the upstream OpenAI provider
  path; Autowake-specific provider behavior is applied only to configured
  custom providers.

### Added

- Migrated automatic resolution for `request_user_input` prompts. The default
  timeout is 60 seconds; `autoResolutionMs` accepts 60 to 240 seconds.
- Migrated `/timer` with recurring and one-time notifications, multiple active
  timers, and interactive edit/cancel management.
- Wake the root agent when a background command completes or a subagent sends a
  message. An active turn is steered immediately; an idle root starts a turn.

### Changed

- `/ps` now reconciles background terminal state through the 0.148.0 app-server
  endpoint, including escalated background terminals.
- Custom non-OpenAI providers use plaintext collaboration messages when the
  provider does not advertise encrypted collaboration support.
- Custom providers receive flat tool names such as `namespace__tool` by
  default. Set `namespace_tools = true` on a compatible provider to retain the
  upstream namespace wrapper.
- Updated the bundled DeepSeek setup for the 0.148.0 provider schema: removed
  obsolete auth fields, retained command-backed API-key loading, and explicitly
  disabled namespace-wrapped tools.

### Timeout behavior

- On timeout, Codex selects the option whose label ends with `(Recommended)`.
- If no option is marked as recommended, Codex selects the first option.
- Keyboard or paste interaction snoozes automatic resolution for the active
  prompt.

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
