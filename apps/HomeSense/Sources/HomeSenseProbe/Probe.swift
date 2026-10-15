import Foundation
import HomeSenseCore

/// Read-only CLI using the exact same session/transport/decoder as the App.
@main
struct Probe {
    @MainActor static func main() async {
        let args = Array(CommandLine.arguments.dropFirst())
        func value(_ flag: String) -> String? {
            guard let index = args.firstIndex(of: flag), args.indices.contains(index + 1) else { return nil }
            return args[index + 1]
        }
        guard let host = value("--host"), let port = value("--port"),
              args.contains("--isolated") || value("--address") != nil else {
            print("Usage: homesense-probe --host HOST --port PORT [--address HEX | --isolated]")
            Foundation.exit(2)
        }
        let session = CommissioningSession(defaults: nil)
        session.host = host; session.port = port
        session.connect()
        for _ in 0..<100 {
            if session.status != .connecting { break }
            try? await Task.sleep(for: .milliseconds(100))
        }
        guard session.status == .connected else { print(session.transcript); Foundation.exit(1) }
        session.isolatedPanel = args.contains("--isolated")
        if let address = value("--address") {
            session.address = address
            session.send(.readAddress, label: "定向地址查询", expectsReply: true)
        } else {
            session.isolatedPanel = true
            session.queryUnknownAddress()
        }
        while session.busy { try? await Task.sleep(for: .milliseconds(50)) }
        let candidate = session.queriedAddress
        if let candidate {
            print("READ_ADDRESS=\(candidate)")
            if candidate.value != 0 {
                session.useAddress(candidate.value)
                session.send(.firmware, label: "固件版本查询", expectsReply: true)
                while session.busy { try? await Task.sleep(for: .milliseconds(50)) }
            }
        }
        print(session.transcript)
        session.disconnect()
        if candidate == nil { Foundation.exit(1) }
    }
}
