# MRLess iOS host

The iPhone is a **full mesh node**, not a remote screen. Rust `Node::start` runs in-process; the existing Solid UI loads in a WKWebView and talks to `ws://127.0.0.1:<ws_port>`.

The mesh stays up **while the app is open**. Leaving the app starts a short `beginBackgroundTask` so an in-flight flush can finish; iOS will still suspend sockets after that. Coming back to the app re-browses Bonjour and redials remembered peers, then flushes the outbox.

UDP broadcast often fails on iPhone — use **mDNS** (`_mrless._tcp`) or add the Mac as a manual peer / invite `LAN_IP:7421` in Settings.

Full **Xcode** is required (Command Line Tools alone cannot sign or install on a device). Select your Team in the project. Free provisioning lasts ~7 days.

Multicast entitlement (`com.apple.developer.networking.multicast`) is **not** in the default entitlements — a free team cannot sign it. Local Network permission + Bonjour (`_mrless._tcp`) plus manual peers are enough for v1. Add multicast later with a paid Apple Developer account if UDP/mDNS is flaky.

## 1. UI bundle

From the UI repo:

```bash
cd ../MRLess
bun run build
rm -rf ../MRLess_server/ios/MRLess/www
cp -R dist ../MRLess_server/ios/MRLess/www
```

## 2. Rust static library

```bash
cd ../MRLess_server
./ios/build-rust.sh
```

This produces `ios/lib/libmrless.a` for device (`aarch64-apple-ios`) and/or simulator.

## 3. Xcode project

```bash
cd ios
open MRLess.xcodeproj
```

(`project.yml` is there if you prefer XcodeGen.) Select your Team, plug in the iPhone, run. First launch asks for **Local Network** permission — accept it.

This host machine needs the full Xcode app, not only Command Line Tools.

## First iPhone ↔ Mac link

Each device is its own node — no separate `mrless` process to babysit.

1. Mac: in the UI repo run `bun run app` (starts node + UI together)
2. iPhone: open MRLess, finish onboarding (node starts inside the app)
3. If they do not find each other: on the phone, Settings → Manual peers → `192.168.x.x:7421`
4. Chat in LAN Mesh

If mDNS/UDP discovery works, skip step 3.
