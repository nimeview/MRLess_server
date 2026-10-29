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
                .frame(minWidth: 880, minHeight: 600)
                .preferredColorScheme(.dark)
        }
        .defaultSize(width: 1100, height: 720)
        .commands {
            CommandGroup(replacing: .newItem) {}
        }
        .onChange(of: scenePhase) { phase in
            if phase == .active {
                NotificationCenter.default.post(name: .mrlessResumeUI, object: nil)
            }
        }
    }
}
