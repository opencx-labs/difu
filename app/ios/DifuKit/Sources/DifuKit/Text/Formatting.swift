import Foundation

/// Short human-readable durations and times, matching the terminal app.
public enum Formatting {
    /// `12s`, `1m 5s`, `2h 3m`.
    public static func elapsed(seconds: Int) -> String {
        let seconds = max(0, seconds)
        if seconds < 60 { return "\(seconds)s" }
        if seconds < 3600 { return "\(seconds / 60)m \(seconds % 60)s" }
        return "\(seconds / 3600)h \((seconds % 3600) / 60)m"
    }

    /// Elapsed time since a Unix-milliseconds timestamp.
    public static func elapsed(since milliseconds: Int, now: Date = Date()) -> String {
        elapsed(seconds: (now.milliseconds - milliseconds) / 1000)
    }

    /// Tool durations: `0.4s`, `12.3s`, `2m 5s`.
    public static func duration(milliseconds: Int) -> String {
        let milliseconds = max(0, milliseconds)
        if milliseconds < 60_000 {
            return String(format: "%.1fs", Double(milliseconds) / 1000)
        }
        return elapsed(seconds: milliseconds / 1000)
    }

    /// Compact relative time: `now`, `5m`, `3h`, `2d`, then a short date.
    public static func relative(_ date: Date, now: Date = Date()) -> String {
        let seconds = Int(now.timeIntervalSince(date))
        if seconds < 45 { return "now" }
        if seconds < 3600 { return "\(max(1, seconds / 60))m" }
        if seconds < 86_400 { return "\(seconds / 3600)h" }
        if seconds < 7 * 86_400 { return "\(seconds / 86_400)d" }
        let sameYear = Calendar.current.isDate(date, equalTo: now, toGranularity: .year)
        return date.formatted(
            sameYear ? .dateTime.month(.abbreviated).day() : .dateTime.year().month(.abbreviated).day())
    }
}
