import Foundation
import Network

/// Publishes this node via Bonjour and browses for peers.
/// Triggers the iOS Local Network permission prompt (UDP broadcast alone often does not).
final class MeshDiscovery: NSObject, NetServiceDelegate {
    static let shared = MeshDiscovery()

    private var publisher: NetService?
    private var browser: NWBrowser?
    private var resolving = [NWConnection]()
    private var meshPort: Int32 = 7421
    private var ourId = ""
    private var ourShort = ""
    private var seen = Set<String>()

    private override init() {
        super.init()
    }

    func start() {
        meshPort = Int32(mrless_mesh_port())
        if meshPort == 0 { meshPort = 7421 }

        if let cstr = mrless_node_id() {
            ourId = String(cString: cstr)
            mrless_string_free(cstr)
        }
        ourShort = ourId.split(separator: "-").first.map(String.init) ?? ""

        publish()
        browse()
    }

    func restart() {
        seen.removeAll()
        browser?.cancel()
        browser = nil
        if publisher == nil {
            publish()
        }
        browse()
        NSLog("MRLess Bonjour restart")
    }

    private func publish() {
        let name = ourShort.isEmpty ? "mrless-ios" : "mrless-\(ourShort)"
        let svc = NetService(
            domain: "local.",
            type: "_mrless._tcp.",
            name: name,
            port: meshPort
        )
        var txt: [String: Data] = [:]
        if !ourId.isEmpty, let data = ourId.data(using: .utf8) {
            txt["id"] = data
        }
        svc.setTXTRecord(NetService.data(fromTXTRecord: txt))
        svc.delegate = self
        svc.publish()
        publisher = svc
        NSLog("MRLess Bonjour published %@ port %d", name, meshPort)
    }

    private func browse() {
        let params = NWParameters()
        params.includePeerToPeer = true
        let browser = NWBrowser(
            for: .bonjour(type: "_mrless._tcp", domain: "local."),
            using: params
        )
        browser.stateUpdateHandler = { state in
            switch state {
            case .ready:
                NSLog("MRLess Bonjour browse ready")
            case .failed(let err):
                NSLog("MRLess Bonjour browse failed: %@", err.localizedDescription)
            default:
                break
            }
        }
        browser.browseResultsChangedHandler = { [weak self] results, _ in
            self?.handle(results: results)
        }
        browser.start(queue: .main)
        self.browser = browser
    }

    private func handle(results: Set<NWBrowser.Result>) {
        for result in results {
            if case let .service(name: name, type: _, domain: _, interface: _) = result.endpoint {
                if !ourShort.isEmpty, name.contains(ourShort) {
                    continue
                }
            }
            resolve(result)
        }
    }

    private func resolve(_ result: NWBrowser.Result) {
        let connection = NWConnection(to: result.endpoint, using: .tcp)
        resolving.append(connection)
        connection.stateUpdateHandler = { [weak self, weak connection] state in
            guard let self, let connection else { return }
            switch state {
            case .ready:
                defer {
                    connection.cancel()
                    self.resolving.removeAll { $0 === connection }
                }
                guard let endpoint = connection.currentPath?.remoteEndpoint,
                      case let .hostPort(host, port) = endpoint
                else { return }

                // Prefer real LAN IPv4; skip link-local / IPv6 noise that fails mesh dial.
                let hostStr: String?
                switch host {
                case .ipv4(let v4):
                    let s = "\(v4)"
                    hostStr = s.hasPrefix("169.254.") ? nil : s
                case .ipv6:
                    hostStr = nil
                case .name(let name, _):
                    hostStr = name
                @unknown default:
                    hostStr = nil
                }
                guard let hostStr else { return }
                self.dial("\(hostStr):\(port.rawValue)")
            case .failed, .cancelled:
                self.resolving.removeAll { $0 === connection }
            default:
                break
            }
        }
        connection.start(queue: .global(qos: .utility))
    }

    private func dial(_ hostPort: String) {
        if seen.contains(hostPort) { return }
        seen.insert(hostPort)
        DispatchQueue.main.asyncAfter(deadline: .now() + 20) { [weak self] in
            self?.seen.remove(hostPort)
        }
        let rc = hostPort.withCString { mrless_connect_peer($0) }
        NSLog("MRLess dial %@ → %d", hostPort, rc)
    }

    func netServiceDidPublish(_ sender: NetService) {
        NSLog("MRLess Bonjour did publish %@", sender.name)
    }

    func netService(_ sender: NetService, didNotPublish errorDict: [String: NSNumber]) {
        NSLog("MRLess Bonjour publish failed: %@", errorDict)
    }
}
