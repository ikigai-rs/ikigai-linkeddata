# Changelog

## ikigai-feed 0.6.2

- Pin feed-rs to 3.0.0 so a fresh dependency resolution builds with the chrono date model.
- Upgrade from ikigai-feed 0.6.1 by updating the dependency to 0.6.2. No persisted state migration is needed.

## 0.6.1

- Refuse invalid optional arguments that previously selected a default silently. Callers sending an unreadable value must provide a valid inline value or omit the argument.
