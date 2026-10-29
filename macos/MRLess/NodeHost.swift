import Foundation
import AppKit

final class NodeHost {
    static let shared = NodeHost()
    private(set) var wsPort: UInt16 = 7422
    private(set) var started = false

    private init() {}

    func start() {
        if started { return }
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("mrless", isDirectory: true)
            // Same instance as `bun run app` / CLI `--instance mac` so history is shared.
            .appendingPathComponent("mac", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)

        let rc = dir.path.withCString { dataDir in
            mrless_start(dataDir, nil)
        }
        if rc != 0 {
            NSLog("mrless_start failed: %d", rc)
            return
        }
        let port = mrless_ws_port()
        if port != 0 {
            wsPort = port
        }
        started = true
        NSLog("MRLess node on ws://127.0.0.1:%u", wsPort)
        MeshDiscovery.shared.start()
    }
}

extension Notification.Name {
    static let mrlessResumeUI = Notification.Name("mrlessResumeUI")
}
