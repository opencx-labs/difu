import Foundation

/// How much space the server has left (`server.disk`).
public enum DiskStatus: String, LenientEnum {
    case ok
    /// Free space is below 15% or 20 GB.
    case low
    /// Free space is below 5% or 5 GB: the server pauses new sessions, moves
    /// and clones until space is freed.
    case critical
    case unknown

    public static let fallback = DiskStatus.unknown

    public var label: String {
        switch self {
        case .ok: "OK"
        case .low: "Low"
        case .critical: "Almost full"
        case .unknown: "Unknown"
        }
    }
}

/// A cleanup the server runs on request (`server.disk.clean`).
public enum DiskCleanAction: String, CaseIterable, Identifiable, Hashable, Sendable {
    /// Removes the worktrees of archived, inactive sessions; their chats,
    /// branches and commits stay. Worktrees with uncommitted, untracked or
    /// ignored files (other than build outputs) are kept.
    case archivedWorktrees
    /// Deletes rebuildable, Git-ignored build folders in idle sessions.
    case buildOutputs
    /// `podman system prune -f`, and unused images other than the sandbox image.
    case sandbox

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .archivedWorktrees: "Remove Archived Worktrees"
        case .buildOutputs: "Clear Build Outputs"
        case .sandbox: "Prune Sandbox Storage"
        }
    }
}

/// The server's disk (`server.disk`): live space on the filesystem holding
/// difu's agents folder, and a breakdown measured in the background.
public struct ServerDisk: Hashable, Sendable, Decodable {
    /// The folder whose filesystem is measured.
    public var path: String?
    public var total: Int64
    public var used: Int64
    /// Space available to difu.
    public var free: Int64
    public var status: DiskStatus
    /// Unix milliseconds the breakdown was measured; nil until the first
    /// measurement finishes (the breakdown is empty until then).
    public var measuredAt: Int?
    /// A measurement is running; the breakdown is the previous one.
    public var measuring: Bool
    public var breakdown: DiskBreakdown

    /// `used / total`, from 0 to 1.
    public var usedFraction: Double {
        guard total > 0 else { return 0 }
        return min(1, max(0, Double(used) / Double(total)))
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        path = f.string("path").flatMap { $0.isEmpty ? nil : $0 }
        total = f.bytes("total") ?? 0
        used = f.bytes("used") ?? 0
        free = f.bytes("free") ?? 0
        status = DiskStatus(lenient: f.string("status"))
        measuredAt = f.timestamp("measuredAt")
        measuring = f.bool("measuring") ?? false
        breakdown = f.value("breakdown", as: DiskBreakdown.self) ?? DiskBreakdown()
    }
}

/// What uses the server's disk, in bytes.
public struct DiskBreakdown: Hashable, Sendable, Decodable {
    /// Session worktrees, biggest first.
    public var sessions: [SessionDiskUsage] = []
    /// The server's base clones.
    public var repositories: Int64 = 0
    public var attachments: Int64 = 0
    public var caches: Int64 = 0
    public var sandbox = SandboxDiskUsage()
    /// The rest of the used space.
    public var other: Int64 = 0

    public init() {}

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        sessions = f.list("sessions", of: SessionDiskUsage.self).sorted { $0.bytes > $1.bytes }
        repositories = f.bytes("repositories") ?? 0
        attachments = f.bytes("attachments") ?? 0
        caches = f.bytes("caches") ?? 0
        sandbox = f.value("sandbox", as: SandboxDiskUsage.self) ?? SandboxDiskUsage()
        other = f.bytes("other") ?? 0
    }

    /// All session worktrees together.
    public var sessionsTotal: Int64 { sessions.map(\.bytes).saturatingSum }

    /// The sessions a cleanup works on, biggest first: archived, inactive ones
    /// for `archivedWorktrees`, idle ones with build outputs for
    /// `buildOutputs`, none for `sandbox`.
    public func sessions(for action: DiskCleanAction) -> [SessionDiskUsage] {
        switch action {
        case .archivedWorktrees: sessions.filter { $0.archived && !$0.active && $0.bytes > 0 }
        case .buildOutputs: sessions.filter { !$0.active && $0.buildOutputs > 0 }
        case .sandbox: []
        }
    }

    /// About how much a cleanup frees; for `sandbox`, at most this much.
    public func expectedSpace(for action: DiskCleanAction) -> Int64 {
        switch action {
        case .archivedWorktrees: sessions(for: action).map(\.bytes).saturatingSum
        case .buildOutputs: sessions(for: action).map(\.buildOutputs).saturatingSum
        case .sandbox: sandbox.available ? sandbox.reclaimable : 0
        }
    }

    /// Whether a cleanup has anything to remove.
    public func canClean(_ action: DiskCleanAction) -> Bool {
        expectedSpace(for: action) > 0
    }
}

/// One session's worktree.
public struct SessionDiskUsage: Identifiable, Hashable, Sendable, Decodable {
    public var id: String
    public var title: String
    public var archived: Bool
    /// Running or waiting; cleanups never touch it.
    public var active: Bool
    public var bytes: Int64
    /// The part of `bytes` that clearing build outputs could free.
    public var buildOutputs: Int64

    public var displayTitle: String {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "New session" : trimmed
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let id = f.string("id"), !id.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "session id"))
        }
        self.id = id
        title = f.string("title") ?? ""
        archived = f.bool("archived") ?? false
        active = f.bool("active") ?? false
        bytes = f.bytes("bytes") ?? 0
        buildOutputs = f.bytes("buildOutputs") ?? 0
    }
}

/// Podman storage; `available` is false when the server has no Podman.
public struct SandboxDiskUsage: Hashable, Sendable, Decodable {
    public var available = false
    public var images: Int64 = 0
    public var containers: Int64 = 0
    public var volumes: Int64 = 0
    /// What a prune could free, at most.
    public var reclaimable: Int64 = 0

    public init() {}

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        available = f.bool("available") ?? false
        images = f.bytes("images") ?? 0
        containers = f.bytes("containers") ?? 0
        volumes = f.bytes("volumes") ?? 0
        reclaimable = f.bytes("reclaimable") ?? 0
    }

    public var total: Int64 { [images, containers, volumes].saturatingSum }
}

/// What a cleanup did (`server.disk.clean`).
public struct DiskCleanResult: Hashable, Sendable, Decodable {
    public var freed: Int64
    /// What was removed and what was kept, and why.
    public var summary: String

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        freed = f.bytes("freed") ?? 0
        summary = f.string("summary")?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }
}

extension Sequence where Element == Int64 {
    /// Sums without trapping on absurd values.
    fileprivate var saturatingSum: Int64 {
        reduce(0) { sum, value in
            let (result, overflow) = sum.addingReportingOverflow(value)
            return overflow ? .max : result
        }
    }
}
