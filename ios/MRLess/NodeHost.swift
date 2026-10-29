import Foundation
import UIKit

final class NodeHost {
    static let shared = NodeHost()
    private(set) var wsPort: UInt16 = 7422
    private(set) var started = false
    private var bgTask: UIBackgroundTaskIdentifier = .invalid

    private init() {}

    func start() {
        if started { return }
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("mrless", isDirectory: true)
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
        NSLog("MRLess node on ws://127.0.0.1:%u (foreground mesh)", wsPort)
        MeshDiscovery.shared.start()
    }

    /// Brief OS hold so an in-flight TCP flush can finish after leaving the app.
    func holdBackground() {
        endBackground()
        bgTask = UIApplication.shared.beginBackgroundTask(withName: "mrless-mesh") { [weak self] in
            self?.endBackground()
        }
    }

    func endBackground() {
        if bgTask != .invalid {
            UIApplication.shared.endBackgroundTask(bgTask)
            bgTask = .invalid
        }
    }

    /// Re-browse Bonjour and redial remembered peers after returning to the app.
    func resumeForeground() {
        endBackground()
        guard started else { return }
        MeshDiscovery.shared.restart()
        let rc = mrless_reconnect_peers()
        NSLog("MRLess foreground revive reconnect=%d", rc)
        // Tell WKWebView to drop a zombie localhost WS and reconnect.
        NotificationCenter.default.post(name: .mrlessResumeUI, object: nil)
    }
}

extension Notification.Name {
    static let mrlessResumeUI = Notification.Name("mrlessResumeUI")
}
