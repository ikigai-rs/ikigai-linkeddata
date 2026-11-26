# Changelog

## 0.7.0

- All six crates move to ikigai-core 0.5.1 and ikigai-vocab 0.5.0. This is a minor release because public APIs carry core types; hosts must upgrade their core dependencies together. Conformance tests use 0.16 and feed tests use ikigai-fs 0.7.
- Feed parsing remains on feed-rs 3.0.0 because 3.1 changes its date model.
- Core 0.5 measures `Invocation::time_left` on operational time. This workspace has no `time_left` callers. No persisted state changes, so no migration is needed.

## 0.6.1

- Refuse invalid optional arguments that previously selected a default silently. Callers sending an unreadable value must provide a valid inline value or omit the argument.
