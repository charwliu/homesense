import SwiftUI
import HomeSenseCore

@main
struct HomeSenseApp: App {
    @StateObject private var session = CommissioningSession()
    @StateObject private var home = HomeServerStore()
    @State private var tab = 0
    @Environment(\.scenePhase) private var phase
    var body: some Scene {
        WindowGroup {
            TabView(selection: $tab) {
                HomeManagementView(home: home)
                    .tabItem { Label("我的家", systemImage: "house") }.tag(0)
                ContentView(session: session)
                    .tabItem { Label("本地调试", systemImage: "wrench.and.screwdriver") }.tag(1)
            }
                .tint(.teal)
                .task { await home.restoreSavedConnection() }
                .onChange(of: tab) { _, tab in
                    if tab == 0 { home.resume() } else { home.pause() }
                }
                .onChange(of: phase) { _, phase in
                    if phase == .background { session.disconnect(); home.pause() }
                    else if phase == .active, tab == 0 { home.resume() }
                }
        }
        #if os(macOS)
        .defaultSize(width: 1100, height: 800)
        #endif
    }
}
