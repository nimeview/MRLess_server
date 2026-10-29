import SwiftUI

@main
struct MRLessApp: App {
    @Environment(\.scenePhase) private var scenePhase

    init() {
        NodeHost.shared.start()
    }

    var body: some Scene {
        WindowGroup {
            WebShellView()
                .ignoresSafeArea()
                .preferredColorScheme(.dark)
        }
        .onChange(of: scenePhase) { phase in
            switch phase {
            case .background:
                NodeHost.shared.holdBackground()
            case .active:
                NodeHost.shared.resumeForeground()
            default:
                break
            }
        }
    }
}
