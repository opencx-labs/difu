import Foundation
import Security

/// Persists the paired computer's credential.
public protocol CredentialStore: Sendable {
    func load() throws -> Credential?
    func save(_ credential: Credential) throws
    func delete() throws
}

public struct CredentialStoreError: Error, LocalizedError, Sendable {
    public var message: String
    public var errorDescription: String? { message }
}

/// Stores the credential as JSON in a generic-password Keychain item.
public struct KeychainCredentialStore: CredentialStore {
    public var service: String
    public var account: String

    public init(service: String = "app.difu.ios.remote", account: String = "paired-computer") {
        self.service = service
        self.account = account
    }

    private var query: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    public func load() throws -> Credential? {
        var request = query
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &item)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = item as? Data else {
            throw CredentialStoreError(message: "Keychain read failed (\(status)).")
        }
        return try JSONDecoder().decode(Credential.self, from: data)
    }

    public func save(_ credential: Credential) throws {
        let data = try JSONEncoder().encode(credential)
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        var status = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            let item = query.merging(attributes) { _, new in new }
            status = SecItemAdd(item as CFDictionary, nil)
        }
        guard status == errSecSuccess else {
            throw CredentialStoreError(message: "Keychain write failed (\(status)).")
        }
    }

    public func delete() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw CredentialStoreError(message: "Keychain delete failed (\(status)).")
        }
    }
}

/// Stores the credential in a JSON file (used by `difu-remote-probe`).
public struct FileCredentialStore: CredentialStore {
    public var url: URL

    public init(url: URL) {
        self.url = url
    }

    public func load() throws -> Credential? {
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(Credential.self, from: Data(contentsOf: url))
    }

    public func save(_ credential: Credential) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        encoder.dateEncodingStrategy = .iso8601
        try encoder.encode(credential).write(to: url, options: .atomic)
        try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
    }

    public func delete() throws {
        if FileManager.default.fileExists(atPath: url.path) {
            try FileManager.default.removeItem(at: url)
        }
    }
}

/// Keeps the credential in memory (tests, previews).
public final class InMemoryCredentialStore: CredentialStore, @unchecked Sendable {
    private let lock = NSLock()
    private var credential: Credential?

    public init(_ credential: Credential? = nil) {
        self.credential = credential
    }

    public func load() throws -> Credential? {
        lock.withLock { credential }
    }

    public func save(_ credential: Credential) throws {
        lock.withLock { self.credential = credential }
    }

    public func delete() throws {
        lock.withLock { credential = nil }
    }
}
