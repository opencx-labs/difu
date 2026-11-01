import Foundation
import Security

/// Persists a single paired computer's credential (`difu-remote-probe`, and the
/// layout the app used before it supported several servers).
public protocol CredentialStore: Sendable {
    func load() throws -> Credential?
    func save(_ credential: Credential) throws
    func delete() throws
}

public struct CredentialStoreError: Error, LocalizedError, Sendable {
    public var message: String
    public var errorDescription: String? { message }
}

// MARK: Secret slots

/// One blob of secret data (a Keychain item, a file, memory).
public protocol SecretSlot: Sendable {
    func read() throws -> Data?
    func write(_ data: Data) throws
    func delete() throws
}

/// A generic-password Keychain item, readable after the first unlock and never
/// synced or migrated to another device.
public struct KeychainSlot: SecretSlot {
    public var service: String
    public var account: String

    public init(service: String, account: String) {
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

    public func read() throws -> Data? {
        var request = query
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &item)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = item as? Data else {
            throw CredentialStoreError(message: "Keychain read failed (\(status)).")
        }
        return data
    }

    public func write(_ data: Data) throws {
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

/// Keeps data in memory (tests, previews).
public final class MemorySlot: SecretSlot, @unchecked Sendable {
    private let lock = NSLock()
    private var data: Data?

    public init(_ data: Data? = nil) {
        self.data = data
    }

    public func read() throws -> Data? {
        lock.withLock { data }
    }

    public func write(_ data: Data) throws {
        lock.withLock { self.data = data }
    }

    public func delete() throws {
        lock.withLock { data = nil }
    }
}

// MARK: Saved servers

/// Persists every saved computer and server, in the order they were added.
public protocol ServerStore: Sendable {
    func load() throws -> [Credential]
    func save(_ servers: [Credential]) throws
}

/// Keeps the saved servers as one JSON array in a secret slot, keyed by server
/// id. On first load it migrates the single credential older versions stored
/// in their own item (`paired-computer`), then deletes that item.
public struct SavedServerStore: ServerStore {
    public static let service = "app.difu.ios.remote"
    public static let account = "paired-servers"
    /// Where versions with a single paired computer kept its credential.
    public static let legacyAccount = "paired-computer"

    let slot: SecretSlot
    let legacy: SecretSlot?

    public init(slot: SecretSlot, legacy: SecretSlot? = nil) {
        self.slot = slot
        self.legacy = legacy
    }

    /// The app's Keychain layout.
    public static func keychain() -> SavedServerStore {
        SavedServerStore(
            slot: KeychainSlot(service: service, account: account),
            legacy: KeychainSlot(service: service, account: legacyAccount))
    }

    public func load() throws -> [Credential] {
        if let data = try slot.read() {
            let servers = (try? JSONDecoder().decode([Lossy<Credential>].self, from: data)) ?? []
            return Self.unique(servers.compactMap(\.value))
        }
        guard let legacy, let data = try legacy.read(),
            let credential = try? JSONDecoder().decode(Credential.self, from: data)
        else { return [] }
        try save([credential])
        try? legacy.delete()
        return [credential]
    }

    public func save(_ servers: [Credential]) throws {
        try slot.write(JSONEncoder().encode(Self.unique(servers)))
    }

    /// Keeps the first credential per server id.
    static func unique(_ servers: [Credential]) -> [Credential] {
        var seen = Set<String>()
        return servers.filter { seen.insert($0.id).inserted }
    }
}

// MARK: Single-credential stores

/// Stores one credential as JSON in a generic-password Keychain item.
public struct KeychainCredentialStore: CredentialStore {
    private let slot: KeychainSlot

    public init(
        service: String = SavedServerStore.service, account: String = SavedServerStore.legacyAccount
    ) {
        slot = KeychainSlot(service: service, account: account)
    }

    public func load() throws -> Credential? {
        guard let data = try slot.read() else { return nil }
        return try JSONDecoder().decode(Credential.self, from: data)
    }

    public func save(_ credential: Credential) throws {
        try slot.write(JSONEncoder().encode(credential))
    }

    public func delete() throws {
        try slot.delete()
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
