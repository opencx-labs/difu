import Foundation

// The terminal's colour themes, read from `Resources/themes.json`, which a test
// in `cli/src/theme.rs` keeps identical to the terminal's. This file and
// `ThemePalette.swift` only need Foundation: `scripts/app-icons.sh` compiles
// them into the icon generator.

/// An sRGB colour.
public struct RGB: Hashable, Sendable, CustomStringConvertible {
    public var red: UInt8
    public var green: UInt8
    public var blue: UInt8

    public init(_ red: UInt8, _ green: UInt8, _ blue: UInt8) {
        self.red = red
        self.green = green
        self.blue = blue
    }

    public init(hex: UInt32) {
        self.init(UInt8((hex >> 16) & 0xFF), UInt8((hex >> 8) & 0xFF), UInt8(hex & 0xFF))
    }

    /// `#rrggbb` or `rrggbb`.
    public init?(hex: String) {
        let digits = hex.hasPrefix("#") ? hex.dropFirst() : Substring(hex)
        guard digits.count == 6, let value = UInt32(digits, radix: 16) else { return nil }
        self.init(hex: value)
    }

    public static let white = RGB(255, 255, 255)
    public static let black = RGB(0, 0, 0)

    /// Lowercase `rrggbb`.
    public var hex: String {
        String(format: "%02x%02x%02x", red, green, blue)
    }

    public var description: String { "#\(hex)" }

    /// WCAG relative luminance: 0 for black, 1 for white.
    public var luminance: Double {
        func linear(_ channel: UInt8) -> Double {
            let value = Double(channel) / 255
            return value <= 0.03928 ? value / 12.92 : pow((value + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
    }

    /// WCAG contrast ratio, from 1 to 21.
    public func contrast(with other: RGB) -> Double {
        let (a, b) = (luminance, other.luminance)
        return (max(a, b) + 0.05) / (min(a, b) + 0.05)
    }

    /// Dark text reads better on this colour than white text does.
    public var isLight: Bool { contrast(with: .black) > contrast(with: .white) }

    /// This colour moved `amount` (0...1) of the way to `other`.
    public func mixed(with other: RGB, amount: Double) -> RGB {
        func channel(_ from: UInt8, _ to: UInt8) -> UInt8 {
            let value = Double(from) + (Double(to) - Double(from)) * amount
            return UInt8(max(0, min(255, value.rounded())))
        }
        return RGB(channel(red, other.red), channel(green, other.green), channel(blue, other.blue))
    }
}

/// One of the terminal's 16 colours, which its colour scheme defines.
public enum ANSIColor: String, CaseIterable, Sendable {
    case black, red, green, yellow, blue, magenta, cyan, gray
    case darkGray = "dark-gray"
    case lightRed = "light-red"
    case lightGreen = "light-green"
    case lightYellow = "light-yellow"
    case lightBlue = "light-blue"
    case lightMagenta = "light-magenta"
    case lightCyan = "light-cyan"
    case white
}

/// A colour as the terminal theme defines it.
public enum TerminalColor: Hashable, Sendable {
    /// The terminal's own foreground or background.
    case terminal
    case rgb(RGB)
    /// One of the terminal's 16 colours.
    case ansi(ANSIColor)
    /// An entry of the 256-colour palette.
    case indexed(UInt8)

    /// The exact colour, when the theme fixes it rather than the terminal.
    /// 256-colour entries from 16 up are standard (a 6×6×6 cube and a grey ramp).
    public var rgb: RGB? {
        switch self {
        case .rgb(let rgb):
            return rgb
        case .indexed(let index) where index >= 232:
            let level = UInt8(8 + 10 * Int(index - 232))
            return RGB(level, level, level)
        case .indexed(let index) where index >= 16:
            let levels: [UInt8] = [0, 95, 135, 175, 215, 255]
            let cube = Int(index - 16)
            return RGB(levels[cube / 36], levels[cube / 6 % 6], levels[cube % 6])
        case .terminal, .ansi, .indexed:
            return nil
        }
    }

    /// The 16 terminal colours, including 256-colour entries 0 to 15.
    public var ansi: ANSIColor? {
        switch self {
        case .ansi(let color): color
        case .indexed(let index) where index < 16: ANSIColor.allCases[Int(index)]
        default: nil
        }
    }
}

extension TerminalColor: Codable {
    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let value = try container.decode(String.self)
        if value == "terminal" {
            self = .terminal
        } else if value.hasPrefix("#"), let rgb = RGB(hex: value) {
            self = .rgb(rgb)
        } else if value.hasPrefix("index:"), let index = UInt8(value.dropFirst("index:".count)) {
            self = .indexed(index)
        } else if let ansi = ANSIColor(rawValue: value) {
            self = .ansi(ansi)
        } else {
            throw DecodingError.dataCorruptedError(
                in: container, debugDescription: "Unknown terminal colour \(value)")
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .terminal: try container.encode("terminal")
        case .rgb(let rgb): try container.encode(rgb.description)
        case .ansi(let color): try container.encode(color.rawValue)
        case .indexed(let index): try container.encode("index:\(index)")
        }
    }
}

/// A terminal colour theme: `cli/src/theme.rs`, role for role.
public struct TerminalTheme: Identifiable, Hashable, Sendable, Codable {
    /// The theme the terminal and the phone start with.
    public static let defaultID = "default"

    public let id: String
    public let name: String
    public let description: String
    public let colors: Colors

    /// The terminal's roles. Themes on the terminal's background leave
    /// `background` as `.terminal`.
    public struct Colors: Hashable, Sendable, Codable {
        public let background: TerminalColor
        public let panel: TerminalColor
        public let text: TerminalColor
        public let dim: TerminalColor
        public let border: TerminalColor
        public let accent: TerminalColor
        public let onAccent: TerminalColor
        public let green: TerminalColor
        public let purple: TerminalColor
        public let yellow: TerminalColor
        public let red: TerminalColor
        public let addBg: TerminalColor
        public let removeBg: TerminalColor
        public let userMessage: TerminalColor
        public let userMessageText: TerminalColor
        public let keyword: TerminalColor
        public let string: TerminalColor
        public let number: TerminalColor
        public let comment: TerminalColor
        public let focusedEntry: TerminalColor
        public let selectedCode: TerminalColor
        public let focusedCode: TerminalColor
    }

    /// The theme is made for a light background. Themes on the terminal's
    /// background tint it for the background they expect (dark, except ANSI
    /// Light), so their code highlight tells.
    public var isLight: Bool {
        (colors.background.rgb ?? colors.focusedCode.rgb ?? .black).isLight
    }

    /// The alternate app icon (`AppIcon-<id>` in the asset catalog); nil for
    /// Default, which uses the primary icon.
    public var iconName: String? {
        id == Self.defaultID ? nil : "AppIcon-\(id)"
    }

    /// The icon's text (the accent) and background colours. Themes on the
    /// terminal's background use the primary icon's near-black (white when
    /// light), and the terminal's own green is iOS's.
    public var iconColors: (text: RGB, background: RGB) {
        let text = colors.accent.rgb ?? colors.accent.ansi.map { $0.systemRGB(light: isLight) } ?? RGB(hex: 0x00FF41)
        let background = colors.background.rgb ?? (isLight ? .white : RGB(hex: 0x050805))
        return (text, background)
    }
}

/// The `themes.json` resource.
public struct TerminalThemeFile: Codable, Sendable {
    public let themes: [TerminalTheme]

    public static func decode(_ data: Data) throws -> [TerminalTheme] {
        try JSONDecoder().decode(TerminalThemeFile.self, from: data).themes
    }
}
