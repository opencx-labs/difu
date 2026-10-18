import Foundation

public enum SessionStatus: String, LenientEnum {
    case starting = "Starting"
    case running = "Running"
    case waiting = "Waiting"
    case idle = "Idle"
    case completed = "Completed"
    case interrupted = "Interrupted"
    case failed = "Failed"
    case unknown = "Unknown"

    public static let fallback = SessionStatus.unknown

    public var isActive: Bool {
        self == .starting || self == .running || self == .waiting
    }

    /// The terminal's label for this status, used when `statusLabel` is missing.
    public var defaultLabel: String {
        switch self {
        case .starting: "Starting"
        case .running: "Working"
        case .waiting: "Needs input"
        case .idle: "Ready"
        case .completed: "Completed"
        case .interrupted: "Interrupted"
        case .failed: "Failed"
        case .unknown: "Unknown"
        }
    }
}

public enum SessionKind: String, LenientEnum {
    case coding = "Coding"
    case orchestrator = "Orchestrator"
    case conflicts = "Conflicts"
    case unknown = "Unknown"

    public static let fallback = SessionKind.unknown
}

public enum EntryRole: String, LenientEnum {
    case user, assistant, reasoning, tool, plan, error, system
    case unknown

    public static let fallback = EntryRole.unknown
}

public enum ToolState: String, LenientEnum {
    case running, done, failed, interrupted
    case unknown

    public static let fallback = ToolState.unknown
}

public enum Delivery: String, LenientEnum {
    case sending, unsent, awaiting
    case unknown

    public static let fallback = Delivery.unknown
}

public enum PlanStatus: String, LenientEnum {
    case completed, inProgress, pending

    public static let fallback = PlanStatus.pending
}

public enum PendingKind: String, LenientEnum {
    case questions, approval, permissions, unsupported
    case unknown

    public static let fallback = PendingKind.unknown
}

public enum DiffStatus: String, LenientEnum {
    case added, deleted, modified, renamed

    public static let fallback = DiffStatus.modified
}
