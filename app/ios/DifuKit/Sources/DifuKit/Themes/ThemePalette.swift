import Foundation

/// An iOS system colour, which follows light and dark appearance by itself.
public enum SystemColor: String, Sendable, CaseIterable {
    case label, secondaryLabel, tertiaryLabel
    case background, secondaryBackground, groupedBackground, secondaryGroupedBackground
    case red, orange, yellow, green, cyan, blue, purple
    case black, white
}

/// How the phone draws one of its colours.
public enum Swatch: Hashable, Sendable {
    case rgb(RGB)
    /// Follows the phone's light or dark appearance.
    case adaptive(light: RGB, dark: RGB)
    case system(SystemColor)
}

/// The colours the iPhone app draws with, mapped from a terminal theme.
///
/// - Themes with their own background paint every screen with it and force
///   dark or light appearance to match.
/// - Default, Amber and Ocean sit on the terminal's background, so they keep
///   iOS's backgrounds and text and follow the phone's appearance. Their accent
///   and green are the terminal's in dark mode and darkened for light mode.
/// - ANSI and ANSI Light use the terminal's own 16 colours, so they use iOS's
///   system colours, in dark and light appearance.
public struct ThemePalette: Hashable, Sendable {
    public enum Scheme: String, Sendable {
        /// Follow the phone's appearance.
        case system, light, dark
    }

    public var scheme: Scheme
    /// Screens paint the theme's backgrounds rather than iOS's.
    public var paintsBackgrounds: Bool

    public var accent: Swatch
    /// Text and icons on accent fills.
    public var onAccent: Swatch
    public var text: Swatch
    public var secondaryText: Swatch
    public var tertiaryText: Swatch

    /// Plain screens: the transcript and plain lists.
    public var background: Swatch
    /// Behind forms and grouped lists.
    public var groupedBackground: Swatch
    /// Rows of grouped lists.
    public var surface: Swatch
    /// Code, diffs, cards and the composer's field.
    public var code: Swatch

    /// The terminal's green: ready sessions, additions, passing checks.
    public var success: Swatch
    /// The terminal's red: failures, deletions, destructive actions.
    public var failure: Swatch
    /// The terminal's yellow: sessions that need input, pending checks.
    public var warning: Swatch
    /// The terminal's purple: merged pull requests and server badges.
    public var violet: Swatch
    /// Notices and renamed files.
    public var info: Swatch

    public init(
        scheme: Scheme, paintsBackgrounds: Bool, accent: Swatch, onAccent: Swatch, text: Swatch,
        secondaryText: Swatch, tertiaryText: Swatch, background: Swatch, groupedBackground: Swatch,
        surface: Swatch, code: Swatch, success: Swatch, failure: Swatch, warning: Swatch, violet: Swatch,
        info: Swatch
    ) {
        self.scheme = scheme
        self.paintsBackgrounds = paintsBackgrounds
        self.accent = accent
        self.onAccent = onAccent
        self.text = text
        self.secondaryText = secondaryText
        self.tertiaryText = tertiaryText
        self.background = background
        self.groupedBackground = groupedBackground
        self.surface = surface
        self.code = code
        self.success = success
        self.failure = failure
        self.warning = warning
        self.violet = violet
        self.info = info
    }

    /// The look difu shipped before themes, which the Default theme maps to.
    public static let standard = ThemePalette(
        scheme: .system, paintsBackgrounds: false,
        accent: .adaptive(light: RGB(hex: 0x008F11), dark: RGB(hex: 0x00FF41)),
        onAccent: .adaptive(light: .white, dark: RGB(hex: 0x0C0E12)),
        text: .system(.label), secondaryText: .system(.secondaryLabel), tertiaryText: .system(.tertiaryLabel),
        background: .system(.background), groupedBackground: .system(.groupedBackground),
        surface: .system(.secondaryGroupedBackground), code: .system(.secondaryBackground),
        success: .adaptive(light: RGB(hex: 0x1E8E5A), dark: RGB(hex: 0x72D8A3)),
        failure: .system(.red), warning: .system(.orange), violet: .system(.purple), info: .system(.blue))

    public init(_ theme: TerminalTheme) {
        let colors = theme.colors
        if let background = colors.background.rgb {
            let light = background.isLight
            let text = colors.text.rgb ?? (light ? .black : .white)
            // Like iOS: light themes put white rows on a greyer page, dark
            // themes lift rows and code a little off the page.
            let tint = background.mixed(with: text, amount: light ? 0.05 : 0.08)
            let accent = Self.swatch(colors.accent)
            self.init(
                scheme: light ? .light : .dark, paintsBackgrounds: true,
                accent: accent, onAccent: Self.swatch(colors.onAccent), text: .rgb(text),
                secondaryText: Self.swatch(colors.dim), tertiaryText: Self.swatch(colors.border),
                background: .rgb(background), groupedBackground: .rgb(light ? tint : background),
                surface: .rgb(light ? background : tint), code: .rgb(tint),
                success: Self.swatch(colors.green), failure: Self.swatch(colors.red),
                warning: Self.swatch(colors.yellow), violet: Self.swatch(colors.purple), info: accent)
        } else if colors.text.rgb == nil {
            var palette = Self.standard
            palette.scheme = theme.isLight ? .light : .dark
            palette.accent = Self.swatch(colors.accent)
            palette.onAccent = Self.swatch(colors.onAccent)
            palette.secondaryText = Self.swatch(colors.dim)
            palette.success = Self.swatch(colors.green)
            palette.failure = Self.swatch(colors.red)
            palette.warning = Self.swatch(colors.yellow)
            palette.violet = Self.swatch(colors.purple)
            self = palette
        } else {
            var palette = Self.standard
            palette.accent = Self.adaptive(colors.accent)
            palette.onAccent = colors.onAccent.rgb.map { .adaptive(light: .white, dark: $0) } ?? palette.onAccent
            palette.success = Self.adaptive(colors.green)
            self = palette
        }
    }

    /// A terminal colour as is; the terminal's own colours become iOS's.
    static func swatch(_ color: TerminalColor) -> Swatch {
        if let rgb = color.rgb { return .rgb(rgb) }
        if let ansi = color.ansi { return .system(ansi.systemColor) }
        return .system(.label)
    }

    /// A colour made for dark backgrounds, with a darker one for light mode.
    static func adaptive(_ color: TerminalColor) -> Swatch {
        guard let rgb = color.rgb else { return swatch(color) }
        return .adaptive(light: lightVariant(of: rgb), dark: rgb)
    }

    /// The hand-picked light colours difu shipped for Default's accent and green.
    static let shippedLightVariants: [RGB: RGB] = [
        RGB(hex: 0x00FF41): RGB(hex: 0x008F11),
        RGB(hex: 0x72D8A3): RGB(hex: 0x1E8E5A),
    ]

    /// `color` darkened until it reads on white (4.5:1), keeping its hue.
    public static func lightVariant(of color: RGB) -> RGB {
        if let shipped = shippedLightVariants[color] { return shipped }
        var variant = color
        var amount = 1.0
        while variant.contrast(with: .white) < 4.5, amount > 0 {
            amount -= 0.01
            variant = RGB.black.mixed(with: color, amount: amount)
        }
        return variant
    }
}

extension ANSIColor {
    /// iOS's counterpart. Yellow becomes orange, iOS's warning colour, which
    /// stays readable on light backgrounds.
    public var systemColor: SystemColor {
        switch self {
        case .black: .black
        case .red, .lightRed: .red
        case .green, .lightGreen: .green
        case .yellow, .lightYellow: .orange
        case .blue, .lightBlue: .blue
        case .magenta, .lightMagenta: .purple
        case .cyan, .lightCyan: .cyan
        case .gray, .darkGray: .secondaryLabel
        case .white: .white
        }
    }

    /// iOS's default value of `systemColor`, for drawing outside UIKit (icons).
    public func systemRGB(light: Bool) -> RGB {
        switch systemColor {
        case .red: RGB(hex: light ? 0xFF3B30 : 0xFF453A)
        case .orange: RGB(hex: light ? 0xFF9500 : 0xFF9F0A)
        case .yellow: RGB(hex: light ? 0xFFCC00 : 0xFFD60A)
        case .green: RGB(hex: light ? 0x34C759 : 0x30D158)
        case .cyan: RGB(hex: light ? 0x32ADE6 : 0x64D2FF)
        case .blue: RGB(hex: light ? 0x007AFF : 0x0A84FF)
        case .purple: RGB(hex: light ? 0xAF52DE : 0xBF5AF2)
        case .white: .white
        case .black: .black
        case .label, .secondaryLabel, .tertiaryLabel, .background, .secondaryBackground,
            .groupedBackground, .secondaryGroupedBackground:
            RGB(hex: 0x8E8E93)
        }
    }
}
