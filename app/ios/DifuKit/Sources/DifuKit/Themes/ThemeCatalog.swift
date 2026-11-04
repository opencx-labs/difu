import Foundation

/// The terminal's themes, bundled as `themes.json`.
public enum ThemeCatalog {
    /// Every theme in the terminal's order, Default first.
    public static let all: [TerminalTheme] = {
        guard let url = Bundle.module.url(forResource: "themes", withExtension: "json"),
            let data = try? Data(contentsOf: url)
        else { return [] }
        return (try? TerminalThemeFile.decode(data)) ?? []
    }()

    /// The theme with `id`; unknown or missing ids use Default, as in the terminal.
    public static func theme(_ id: String?) -> TerminalTheme? {
        all.first { $0.id == id } ?? all.first { $0.id == TerminalTheme.defaultID }
    }
}
