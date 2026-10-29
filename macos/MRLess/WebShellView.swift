import AppKit
import SwiftUI
import WebKit

struct WebShellView: NSViewRepresentable {
    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeNSView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        let scheme = BundleSchemeHandler()
        context.coordinator.scheme = scheme
        config.setURLSchemeHandler(scheme, forURLScheme: "mrless")
        config.defaultWebpagePreferences.allowsContentJavaScript = true
        config.userContentController.add(context.coordinator, name: "saveFile")

        let port = NodeHost.shared.wsPort
        let js = WKUserScript(
            source: "window.__MRLESS_WS = 'ws://127.0.0.1:\(port)'; window.__MRLESS_DESKTOP = true;",
            injectionTime: .atDocumentStart,
            forMainFrameOnly: true
        )
        config.userContentController.addUserScript(js)

        let view = WKWebView(frame: .zero, configuration: config)
        view.setValue(false, forKey: "drawsBackground")
        if #available(macOS 13.3, *) {
            view.isInspectable = true
        }
        view.navigationDelegate = context.coordinator
        context.coordinator.webView = view
        view.load(URLRequest(url: URL(string: "mrless://app/index.html")!))
        return view
    }

    func updateNSView(_ nsView: WKWebView, context: Context) {}

    final class Coordinator: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
        var scheme: BundleSchemeHandler?
        weak var webView: WKWebView?
        private var resumeObserver: NSObjectProtocol?

        override init() {
            super.init()
            resumeObserver = NotificationCenter.default.addObserver(
                forName: .mrlessResumeUI,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                self?.nudgeWebSocket()
            }
        }

        deinit {
            if let resumeObserver {
                NotificationCenter.default.removeObserver(resumeObserver)
            }
        }

        func nudgeWebSocket() {
            guard let webView else { return }
            let js = "window.__MRLESS_RESUME__ && window.__MRLESS_RESUME__();"
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                webView.evaluateJavaScript(js, completionHandler: nil)
            }
        }

        func userContentController(
            _ userContentController: WKUserContentController,
            didReceive message: WKScriptMessage
        ) {
            guard message.name == "saveFile",
                  let body = message.body as? [String: Any],
                  let fileName = body["fileName"] as? String,
                  let dataB64 = body["dataB64"] as? String,
                  let data = Data(base64Encoded: dataB64)
            else { return }

            let safe = (fileName as NSString).lastPathComponent
            let panel = NSSavePanel()
            panel.nameFieldStringValue = safe.isEmpty ? "file.bin" : safe
            panel.begin { resp in
                guard resp == .OK, let url = panel.url else { return }
                try? data.write(to: url, options: .atomic)
            }
        }
    }
}
