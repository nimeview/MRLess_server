# MRLess node

Rust node for MRLess, an experimental messenger for local and invite-based
mesh networks. There is no central server: every device runs its own node and
stores its own identity, messages, contacts, groups, and file cache.

## Responsibilities

- UDP and mDNS discovery on the local network
- TCP peer links with flood/relay routing and hop limits
- WebSocket API for the UI
- Local storage and an outbox for retryable delivery
- Invite codes for peers outside the current LAN
- Native library and application shells for macOS, iOS, Android, and Windows

The companion SolidJS client lives in
[MRLess UI](https://github.com/nimeview/MRLess).

## Quick start

```bash
cargo run --release -- --name Alice
```

For the normal desktop workflow, keep both repositories next to each other and
run `bun run app` from the UI repository. It builds this node when necessary,
then starts the UI and connects it to the local WebSocket endpoint.

## Platform builds

The repository contains the native shells and build scripts for macOS, iOS,
Android, and Windows. The full workflow is documented in the UI repository's
[build guide](https://github.com/nimeview/MRLess/blob/workspace-refresh/docs/build.md).

Examples:

```bash
./scripts/build-macos.sh
./scripts/build-ios.sh
./scripts/build-android.sh
./scripts/build-windows.sh
```

Platform builds require their native toolchains. Apple builds require Xcode;
Android builds require the Android SDK and NDK.

## Status

Personal experimental project. The wire protocol is currently version 3 and
the protocol, storage format, and platform packaging may change.
