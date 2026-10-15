import Foundation
import Security

public protocol HomeCredentialStore: Sendable {
    func load(for origin: String) async throws -> String?
    func save(_ token: String, for origin: String) async throws
    func remove(for origin: String) async throws
}

public enum HomeCredentialError: LocalizedError, Sendable {
    case keychain(OSStatus), invalidData
    public var errorDescription: String? {
        switch self {
        case .keychain(let status): "系统 Keychain 操作失败（\(status)），请解锁设备后重试。"
        case .invalidData: "系统 Keychain 中的凭证格式无效，请移除本机凭证后重新登录。"
        }
    }
}

/// Local device only. The HTTPS origin, including port, is the account scope.
public actor HomeKeychainCredentialStore: HomeCredentialStore {
    public init() {}
    private func query(_ origin: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: "com.ngboss.homesense.authentication",
         kSecAttrAccount as String: origin,
         kSecAttrSynchronizable as String: false]
    }
    public func load(for origin: String) throws -> String? {
        var query = query(origin)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess else { throw HomeCredentialError.keychain(status) }
        guard let data = result as? Data, let token = String(data: data, encoding: .utf8), !token.isEmpty,
              !token.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw HomeCredentialError.invalidData }
        return token
    }
    public func save(_ token: String, for origin: String) throws {
        let query = query(origin), data = Data(token.utf8)
        var value: [String: Any] = [kSecValueData as String: data]
        #if !os(macOS)
        value[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        #endif
        // The macOS file-based login keychain is local and explicitly non-sync.
        // Its items do not accept iOS accessibility classes without DP entitlements.
        let status = SecItemUpdate(query as CFDictionary, value as CFDictionary)
        if status == errSecItemNotFound {
            var attributes = query
            attributes.merge(value) { _, new in new }
            attributes[kSecAttrLabel as String] = "HomeSense · \(origin)"
            let added = SecItemAdd(attributes as CFDictionary, nil)
            guard added == errSecSuccess else { throw HomeCredentialError.keychain(added) }
        } else if status != errSecSuccess { throw HomeCredentialError.keychain(status) }
    }
    public func remove(for origin: String) throws {
        let status = SecItemDelete(query(origin) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw HomeCredentialError.keychain(status) }
    }
}
