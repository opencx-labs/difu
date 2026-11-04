import DifuKit
import SwiftUI

/// Settings › Appearance › Theme: the terminal's themes, each with its app icon.
struct ThemeSettingsView: View {
    private var store: ThemeStore { .shared }

    var body: some View {
        ThemedForm {
            Section {
                ForEach(ThemeCatalog.all) { theme in
                    Button {
                        store.select(theme)
                    } label: {
                        ThemeRow(theme: theme, selected: theme.id == store.id)
                    }
                }
            } footer: {
                VStack(alignment: .leading, spacing: 6) {
                    Text("The terminal's themes. Each one changes the app icon too, which iOS confirms with an alert.")
                    if let error = store.iconError {
                        Text("Couldn't change the icon: \(error)")
                            .foregroundStyle(.failure)
                    }
                }
            }
        }
        .navigationTitle("Theme")
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// A theme with its icon and palette.
private struct ThemeRow: View {
    let theme: TerminalTheme
    let selected: Bool

    var body: some View {
        HStack(spacing: 12) {
            ThemeIcon(theme: theme)
            VStack(alignment: .leading, spacing: 3) {
                // Colours, not hierarchical styles, which would follow the button's tint.
                Text(theme.name)
                    .foregroundStyle(Color.primary)
                Text(theme.description)
                    .font(.caption)
                    .foregroundStyle(Color.secondary)
                ThemeSwatches(theme: theme)
                    .padding(.top, 2)
            }
            Spacer(minLength: 8)
            Image(systemName: "checkmark")
                .font(.body.weight(.semibold))
                .foregroundStyle(Color.accent)
                .opacity(selected ? 1 : 0)
        }
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }
}

/// The theme's app icon in miniature: "difu" in its accent on its background.
private struct ThemeIcon: View {
    let theme: TerminalTheme

    var body: some View {
        let colors = theme.iconColors
        RoundedRectangle(cornerRadius: 10, style: .continuous)
            .fill(Color(.rgb(colors.background)))
            .overlay {
                Text("difu")
                    .font(.system(size: 11, weight: .semibold, design: .monospaced))
                    .foregroundStyle(Color(.rgb(colors.text)))
            }
            .overlay {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(Color.primary.opacity(0.12))
            }
            .frame(width: 44, height: 44)
    }
}

/// Dots for the theme's accent, green, yellow, red and purple, as the phone
/// draws them in the theme's appearance.
private struct ThemeSwatches: View {
    let theme: TerminalTheme
    @Environment(\.colorScheme) private var current

    var body: some View {
        let palette = ThemePalette(theme)
        let swatches = [palette.accent, palette.success, palette.warning, palette.failure, palette.violet]
        HStack(spacing: 4) {
            ForEach(swatches.indices, id: \.self) { index in
                Circle()
                    .fill(Color(swatches[index]))
                    .frame(width: 9, height: 9)
            }
        }
        .environment(\.colorScheme, scheme(palette))
        .accessibilityHidden(true)
    }

    private func scheme(_ palette: ThemePalette) -> ColorScheme {
        switch palette.scheme {
        case .system: current
        case .light: .light
        case .dark: .dark
        }
    }
}
