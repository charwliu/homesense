import Foundation
import Combine

/// A rename is confirmed only by the HomeKit name read back after its callback.
@MainActor public final class HomeAccessoryNameEditor: ObservableObject {
    @Published public var name: String
    @Published public private(set) var saving = false
    @Published public private(set) var error: String?
    public private(set) var original: String
    public init(name: String) { self.name = name; original = name }
    public var proposed: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }
    public var canSave: Bool { !saving && !proposed.isEmpty && proposed != original }

    public func save(using update: (String) async throws -> String) async -> Bool {
        guard canSave else { return false }
        let requested = proposed
        saving = true; error = nil
        defer { saving = false }
        do {
            let observed = try await update(requested)
            guard observed == requested else {
                error = "Apple 家庭读回的名称仍为“\(observed)”，尚未确认保存。不会自动重试。"
                return false
            }
            original = observed; name = observed
            return true
        } catch {
            let detail = error as NSError
            self.error = "名称未确认保存：\(error.localizedDescription)（\(detail.domain) / \(detail.code)）。不会自动重试。"
            return false
        }
    }
}
