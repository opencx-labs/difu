import Foundation

// Prints `<icon set> <text hex> <background hex>` for the primary icon (Default)
// and each theme's alternate icon. app-icons.sh compiles it with DifuKit's
// TerminalTheme.swift and ThemePalette.swift, so the colours match the app's.
@main
struct ThemeIcons {
    static func main() throws {
        let path = CommandLine.arguments.dropFirst().first ?? "themes.json"
        let themes = try TerminalThemeFile.decode(Data(contentsOf: URL(fileURLWithPath: path)))
        for theme in themes {
            let colors = theme.iconColors
            print(theme.iconName ?? "AppIcon", colors.text.hex, colors.background.hex)
        }
    }
}
