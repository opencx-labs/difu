import DifuKit
import SwiftUI

extension Font {
    static let mono = Font.system(.footnote, design: .monospaced)
    static let monoCaption = Font.system(.caption, design: .monospaced)
    static let monoBody = Font.system(.callout, design: .monospaced)
}

// `Color.accent` (matrix green), `.onAccent` (ink on accent fills) and
// `.success` (the terminal's softer green) are generated from the asset catalog.
extension Color {
    static let addition = Color.success
    static let deletion = Color.red
    static let codeBackground = Color(.secondarySystemBackground)
    static let cardBackground = Color(.secondarySystemGroupedBackground)
}

/// How a session status is drawn, matching the terminal's colours.
enum StatusTone: Equatable {
    case working, needsInput, ready, failed, interrupted, neutral

    init(_ summary: SessionSummary) {
        switch summary.statusLabel.lowercased() {
        case "failed": self = .failed
        case "needs input": self = .needsInput
        case "ready", "completed": self = .ready
        case "working", "starting": self = .working
        case "interrupted": self = .interrupted
        default:
            switch summary.status {
            case .failed: self = .failed
            case .waiting: self = .needsInput
            case .idle, .completed: self = .ready
            case .running, .starting: self = .working
            case .interrupted: self = .interrupted
            case .unknown: self = summary.active ? .working : .neutral
            }
        }
    }

    var color: Color {
        switch self {
        case .working: .accentColor
        case .needsInput: .orange
        case .ready: .success
        case .failed: .red
        case .interrupted, .neutral: .secondary
        }
    }
}

/// Small capsule label used for statuses and counts.
struct Pill: View {
    let text: String
    var systemImage: String?
    var color: Color = .secondary

    var body: some View {
        HStack(spacing: 3) {
            if let systemImage {
                Image(systemName: systemImage).imageScale(.small)
            }
            Text(text)
        }
        .font(.caption2.weight(.semibold))
        .foregroundStyle(color)
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(color.opacity(0.14), in: Capsule())
    }
}

/// `+12 −3` in green and red.
struct DiffStat: View {
    let additions: Int
    let deletions: Int

    var body: some View {
        HStack(spacing: 4) {
            Text("+\(additions)").foregroundStyle(Color.addition)
            Text("−\(deletions)").foregroundStyle(Color.deletion)
        }
        .font(.monoCaption)
        .monospacedDigit()
    }
}

/// A live `12s` / `1m 5s` counter.
struct ElapsedText: View {
    let since: Int

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            Text(Formatting.elapsed(since: since, now: context.date))
                .monospacedDigit()
        }
    }
}

/// A dot that pulses while something is in progress.
struct PulsingDot: View {
    var color: Color = .accentColor
    var size: CGFloat = 7

    var body: some View {
        PhaseAnimator([0.3, 1.0]) { opacity in
            Circle().fill(color).frame(width: size, height: size).opacity(opacity)
        } animation: { _ in
            .easeInOut(duration: 0.8)
        }
    }
}

/// Monospace text in a rounded box that scrolls horizontally.
struct CodeBlock: View {
    let text: String
    var font: Font = .mono
    var color: Color = .primary

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(text)
                .font(font)
                .foregroundStyle(color)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .padding(10)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 8))
    }
}

/// Tool output: a plain code block, or a fixed-height scrolling box when long.
struct OutputBlock: View {
    let text: String
    var maxLines = 24

    var body: some View {
        let lines = text.reduce(into: 1) { count, character in
            if character == "\n" { count += 1 }
        }
        if lines <= maxLines {
            CodeBlock(text: text)
        } else {
            ScrollView([.horizontal, .vertical]) {
                Text(text)
                    .font(.mono)
                    .textSelection(.enabled)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(height: 360)
            .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 8))
        }
    }
}
