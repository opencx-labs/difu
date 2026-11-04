import DifuKit
import Observation
import SwiftUI
import UIKit

/// The theme chosen in Settings › Appearance › Theme, saved on the phone. The
/// themes are the terminal's (DifuKit's `themes.json`), and each one but
/// Default has its own app icon.
@Observable
final class ThemeStore: @unchecked Sendable {
    static let shared = ThemeStore()
    static let defaultsKey = "theme"

    /// nil only if the bundled themes failed to load.
    private(set) var theme: TerminalTheme?
    private(set) var colors: ThemeColors
    /// Why iOS refused the last icon change.
    private(set) var iconError: String?

    @ObservationIgnored private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        let theme = ThemeCatalog.theme(defaults.string(forKey: Self.defaultsKey))
        self.theme = theme
        colors = ThemeColors(theme.map(ThemePalette.init) ?? .standard)
    }

    var id: String { theme?.id ?? TerminalTheme.defaultID }

    /// Applies and saves `theme`, and switches the app icon to match (iOS
    /// confirms icon changes with an alert).
    @MainActor func select(_ theme: TerminalTheme) {
        guard theme.id != id else { return }
        defaults.set(theme.id, forKey: Self.defaultsKey)
        self.theme = theme
        colors = ThemeColors(ThemePalette(theme))
        applyWindowTint()
        setIcon(theme.iconName)
    }

    /// UIKit alerts, menus and the lock window take the accent from their window.
    @MainActor func applyWindowTint() {
        let tint = colors.palette.accent.uiColor
        for scene in UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }) {
            for window in scene.windows {
                window.tintColor = tint
            }
        }
    }

    @MainActor private func setIcon(_ name: String?) {
        let app = UIApplication.shared
        guard app.supportsAlternateIcons, app.alternateIconName != name else { return }
        Task { @MainActor in
            do {
                try await app.setAlternateIconName(name)
                iconError = nil
            } catch {
                iconError = error.localizedDescription
            }
        }
    }
}

/// A theme palette as SwiftUI colours.
struct ThemeColors {
    let palette: ThemePalette
    let accent: Color
    let onAccent: Color
    let text: Color
    let background: Color
    let groupedBackground: Color
    let surface: Color
    let code: Color
    let success: Color
    let failure: Color
    let warning: Color
    let violet: Color
    let info: Color

    init(_ palette: ThemePalette) {
        self.palette = palette
        accent = Color(palette.accent)
        onAccent = Color(palette.onAccent)
        text = Color(palette.text)
        background = Color(palette.background)
        groupedBackground = Color(palette.groupedBackground)
        surface = Color(palette.surface)
        code = Color(palette.code)
        success = Color(palette.success)
        failure = Color(palette.failure)
        warning = Color(palette.warning)
        violet = Color(palette.violet)
        info = Color(palette.info)
    }

    /// The appearance the theme needs; nil follows the phone.
    var scheme: ColorScheme? {
        switch palette.scheme {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }

    var paintsBackgrounds: Bool { palette.paintsBackgrounds }
}

extension Swatch {
    var uiColor: UIColor {
        switch self {
        case .rgb(let rgb):
            UIColor(rgb)
        case .adaptive(let light, let dark):
            UIColor { traits in traits.userInterfaceStyle == .dark ? UIColor(dark) : UIColor(light) }
        case .system(let color):
            color.uiColor
        }
    }
}

extension UIColor {
    convenience init(_ rgb: RGB) {
        self.init(
            red: CGFloat(rgb.red) / 255, green: CGFloat(rgb.green) / 255, blue: CGFloat(rgb.blue) / 255,
            alpha: 1)
    }
}

extension SystemColor {
    var uiColor: UIColor {
        switch self {
        case .label: .label
        case .secondaryLabel: .secondaryLabel
        case .tertiaryLabel: .tertiaryLabel
        case .background: .systemBackground
        case .secondaryBackground: .secondarySystemBackground
        case .groupedBackground: .systemGroupedBackground
        case .secondaryGroupedBackground: .secondarySystemGroupedBackground
        case .red: .systemRed
        case .orange: .systemOrange
        case .yellow: .systemYellow
        case .green: .systemGreen
        case .cyan: .systemCyan
        case .blue: .systemBlue
        case .purple: .systemPurple
        case .black: .black
        case .white: .white
        }
    }
}

extension Color {
    init(_ swatch: Swatch) {
        self.init(uiColor: swatch.uiColor)
    }
}

extension View {
    /// The theme's accent and appearance, for a window's root view.
    func themed() -> some View {
        modifier(ThemedRoot())
    }

    /// Paints the theme's background behind a list, form or scroll view, where
    /// the theme has its own; other themes keep iOS's.
    func themedBackground(_ kind: ScreenBackground = .grouped) -> some View {
        modifier(ThemedBackground(kind: kind))
    }

    /// Paints grouped list rows with the theme's row colour. Apply to rows or
    /// sections; `ThemedForm` and `ThemedList` do it for all of theirs.
    func themedRows() -> some View {
        let colors = ThemeStore.shared.colors
        return listRowBackground(colors.paintsBackgrounds ? colors.surface : nil)
    }
}

enum ScreenBackground {
    /// Forms and grouped lists.
    case grouped
    /// The transcript, diffs and other plain screens.
    case plain
}

private struct ThemedRoot: ViewModifier {
    func body(content: Content) -> some View {
        let colors = ThemeStore.shared.colors
        content
            .tint(colors.accent)
            .preferredColorScheme(colors.scheme)
            .onAppear { ThemeStore.shared.applyWindowTint() }
    }
}

private struct ThemedBackground: ViewModifier {
    let kind: ScreenBackground

    func body(content: Content) -> some View {
        let colors = ThemeStore.shared.colors
        content
            .scrollContentBackground(colors.paintsBackgrounds ? .hidden : .automatic)
            .background {
                if colors.paintsBackgrounds {
                    (kind == .grouped ? colors.groupedBackground : colors.background)
                        .ignoresSafeArea()
                }
            }
    }
}

/// A `Form` in the theme's colours.
struct ThemedForm<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        Form { content.themedRows() }
            .themedBackground()
    }
}

/// A grouped `List` in the theme's colours.
struct ThemedList<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        List { content.themedRows() }
            .themedBackground()
    }
}
