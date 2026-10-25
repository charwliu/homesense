import SwiftUI
import HomeSenseCore

@main
struct HomeSenseApp: App {
    @StateObject private var session = CommissioningSession()
    @StateObject private var home = HomeServerStore()
    @StateObject private var daily = HomeKitDailyStore()
    @State private var tab = 0
    @Environment(\.scenePhase) private var phase
    var body: some Scene {
        WindowGroup {
            TabView(selection: $tab) {
                HomeKitDailyView(store: daily)
                    .tabItem { Label("家庭", systemImage: "house") }.tag(0)
                HomeManagementView(home: home, configurationOnly: true)
                    .tabItem { Label("配置管理", systemImage: "slider.horizontal.3") }.tag(1)
                ContentView(session: session)
                    .tabItem { Label("本地调试", systemImage: "wrench.and.screwdriver") }.tag(2)
            }
            .tint(.teal)
            .task { await activate() }
            .onChange(of: tab) { _, _ in Task { await activate() } }
            .onChange(of: phase) { _, _ in Task { await activate() } }
        }
    }
    @MainActor private func activate() async {
        let active = phase != .background
        daily.setActive(active && tab == 0)
        home.setApplicationActive(active && tab == 1)
        if active && tab == 1 {
            home.resume()
            await home.restoreSavedConnection()
            home.selectInterfaceMode(.management)
        }
        if !active || tab != 2 { session.disconnect() }
    }
}
