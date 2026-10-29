import SwiftUI
import UIKit
import WebKit

struct WebShellView: UIViewRepresentable {
    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        let scheme = BundleSchemeHandler()
        context.coordinator.scheme = scheme
        config.setURLSchemeHandler(scheme, forURLScheme: "mrless")
        config.defaultWebpagePreferences.allowsContentJavaScript = true
        config.userContentController.add(context.coordinator, name: "saveFile")

        let port = NodeHost.shared.wsPort
        let js = WKUserScript(
            source: "window.__MRLESS_WS = 'ws://127.0.0.1:\(port)'; window.__MRLESS_IOS = true;",
            injectionTime: .atDocumentStart,
            forMainFrameOnly: true
        )
        config.userContentController.addUserScript(js)

        let view = MeshWebView(frame: .zero, configuration: config)
        view.isOpaque = true
        view.backgroundColor = UIColor(red: 0.05, green: 0.05, blue: 0.05, alpha: 1)
        view.scrollView.backgroundColor = view.backgroundColor
        // Pin the shell: bounce/pan belongs to in-app scrollers, not the page.
        view.scrollView.isScrollEnabled = false
        view.scrollView.bounces = false
        view.scrollView.alwaysBounceVertical = false
        view.scrollView.alwaysBounceHorizontal = false
        view.scrollView.contentInsetAdjustmentBehavior = .never
        view.scrollView.contentInset = .zero
        view.scrollView.scrollIndicatorInsets = .zero
        if #available(iOS 16.4, *) {
            view.isInspectable = true
        }
        view.navigationDelegate = context.coordinator
        context.coordinator.webView = view
        view.load(URLRequest(url: URL(string: "mrless://app/index.html")!))
        return view
    }

    func updateUIView(_ uiView: WKWebView, context: Context) {
        (uiView as? MeshWebView)?.pushSafeArea()
    }

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
            // Small delay so the node TCP listener is awake after suspend.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
                webView.evaluateJavaScript(js, completionHandler: nil)
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                webView.evaluateJavaScript(js, completionHandler: nil)
            }
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            (webView as? MeshWebView)?.pushSafeArea()
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
            let tmp = FileManager.default.temporaryDirectory.appendingPathComponent(safe.isEmpty ? "file.bin" : safe)
            do {
                try data.write(to: tmp, options: .atomic)
            } catch {
                NSLog("saveFile write failed: %@", error.localizedDescription)
                return
            }

            DispatchQueue.main.async { [weak self] in
                guard let presenter = self?.topViewController() else { return }
                let activity = UIActivityViewController(activityItems: [tmp], applicationActivities: nil)
                if let pop = activity.popoverPresentationController {
                    pop.sourceView = presenter.view
                    pop.sourceRect = CGRect(
                        x: presenter.view.bounds.midX,
                        y: presenter.view.bounds.midY,
                        width: 1,
                        height: 1
                    )
                }
                presenter.present(activity, animated: true)
            }
        }

        private func topViewController() -> UIViewController? {
            let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            let window = scenes.flatMap(\.windows).first(where: \.isKeyWindow)
                ?? scenes.flatMap(\.windows).first
            var top = window?.rootViewController
            while let presented = top?.presentedViewController {
                top = presented
            }
            return top
        }
    }
}

/// Custom scheme pages often get `env(safe-area-inset-*) = 0`. Push real insets as CSS vars.
final class MeshWebView: WKWebView {
    private var lastInsets = UIEdgeInsets(top: -1, left: -1, bottom: -1, right: -1)

    override func safeAreaInsetsDidChange() {
        super.safeAreaInsetsDidChange()
        pushSafeArea()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        pushSafeArea()
    }

    func pushSafeArea() {
        let i = safeAreaInsets
        if i == lastInsets { return }
        lastInsets = i
        let js = """
        (function(){
          var r = document.documentElement;
          if (!r || !r.style) return;
          r.style.setProperty('--sat', '\(i.top)px');
          r.style.setProperty('--sab', '\(i.bottom)px');
          r.style.setProperty('--sal', '\(i.left)px');
          r.style.setProperty('--sar', '\(i.right)px');
        })();
        """
        evaluateJavaScript(js, completionHandler: nil)
    }
}
