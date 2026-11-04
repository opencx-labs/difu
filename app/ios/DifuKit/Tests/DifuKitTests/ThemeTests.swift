import XCTest

@testable import DifuKit

final class ThemeTests: XCTestCase {
    private func theme(_ id: String) throws -> TerminalTheme {
        try XCTUnwrap(ThemeCatalog.all.first { $0.id == id }, "no theme \(id)")
    }

    func testBundlesEveryTerminalThemeInOrder() throws {
        let ids = ThemeCatalog.all.map(\.id)
        XCTAssertEqual(ids, [
            "default", "amber", "ocean", "ansi", "ansi-light", "dracula", "nord", "gruvbox-dark",
            "catppuccin-mocha", "tokyo-night", "one-dark", "monokai", "solarized-dark", "rose-pine",
            "kanagawa", "everforest-dark", "solarized-light", "github-light", "catppuccin-latte",
            "gruvbox-light",
        ])
        let rose = try theme("rose-pine")
        XCTAssertEqual(rose.name, "Rosé Pine")
        XCTAssertEqual(rose.description, "Muted rose and pine at dusk")
        XCTAssertEqual(try theme("nord").colors.accent, .rgb(RGB(hex: 0x88C0D0)))
        XCTAssertEqual(try theme("default").colors.background, .terminal)
        XCTAssertEqual(try theme("default").colors.userMessageText, .ansi(.white))
        XCTAssertEqual(try theme("ansi").colors.dim, .ansi(.darkGray))
        XCTAssertEqual(try theme("ansi-light").colors.focusedCode, .indexed(255))
    }

    func testUnknownThemesUseDefault() throws {
        XCTAssertEqual(ThemeCatalog.theme("gruvbox-dark")?.id, "gruvbox-dark")
        XCTAssertEqual(ThemeCatalog.theme("removed-theme")?.id, "default")
        XCTAssertEqual(ThemeCatalog.theme(nil)?.id, "default")
    }

    func testDecodesEveryColourForm() throws {
        let json = ##"["terminal", "#0A0b0C", "dark-gray", "light-magenta", "index:22", "index:244"]"##
        let colors = try JSONDecoder().decode([TerminalColor].self, from: Data(json.utf8))
        XCTAssertEqual(colors, [
            .terminal, .rgb(RGB(10, 11, 12)), .ansi(.darkGray), .ansi(.lightMagenta), .indexed(22),
            .indexed(244),
        ])
        let encoded = try JSONEncoder().encode(colors)
        XCTAssertEqual(try JSONDecoder().decode([TerminalColor].self, from: encoded), colors)

        for bad in [#"["purple"]"#, ##"["#12345"]"##, #"["index:256"]"#] {
            XCTAssertThrowsError(try JSONDecoder().decode([TerminalColor].self, from: Data(bad.utf8)), bad)
        }
        // A theme missing a role doesn't load.
        let partial = #"{"themes": [{"id": "x", "name": "X", "description": "", "colors": {"text": "terminal"}}]}"#
        XCTAssertThrowsError(try TerminalThemeFile.decode(Data(partial.utf8)))
    }

    func testResolvesThe256ColourPalette() {
        XCTAssertEqual(TerminalColor.indexed(22).rgb, RGB(hex: 0x005F00))
        XCTAssertEqual(TerminalColor.indexed(194).rgb, RGB(hex: 0xD7FFD7))
        XCTAssertEqual(TerminalColor.indexed(52).rgb, RGB(hex: 0x5F0000))
        XCTAssertEqual(TerminalColor.indexed(232).rgb, RGB(hex: 0x080808))
        XCTAssertEqual(TerminalColor.indexed(255).rgb, RGB(hex: 0xEEEEEE))
        XCTAssertNil(TerminalColor.indexed(12).rgb)
        XCTAssertEqual(TerminalColor.indexed(12).ansi, .lightBlue)
        XCTAssertNil(TerminalColor.terminal.rgb)
    }

    func testThemesAreLightOrDarkLikeTheirBackground() {
        let light = ThemeCatalog.all.filter(\.isLight).map(\.id)
        XCTAssertEqual(light, ["ansi-light", "solarized-light", "github-light", "catppuccin-latte", "gruvbox-light"])
    }

    func testDefaultKeepsTheShippedLook() throws {
        XCTAssertEqual(ThemePalette(try theme("default")), .standard)
    }

    func testAccentThemesFollowThePhoneWithReadableLightColours() throws {
        for (id, accent) in [("amber", RGB(hex: 0xFFB000)), ("ocean", RGB(hex: 0x38BDF8))] {
            let palette = ThemePalette(try theme(id))
            XCTAssertEqual(palette.scheme, .system, id)
            XCTAssertFalse(palette.paintsBackgrounds, id)
            guard case .adaptive(let light, let dark) = palette.accent else {
                return XCTFail("\(id) accent isn't adaptive: \(palette.accent)")
            }
            XCTAssertEqual(dark, accent, id)
            XCTAssertGreaterThanOrEqual(light.contrast(with: .white), 4.5, id)
            XCTAssertLessThan(light.contrast(with: .white), 5, "\(id) darkens more than needed")
            XCTAssertEqual(palette.onAccent, .adaptive(light: .white, dark: RGB(hex: 0x0C0E12)), id)
            // Default's green, with its shipped light colour.
            XCTAssertEqual(palette.success, ThemePalette.standard.success, id)
            XCTAssertEqual(palette.background, .system(.background), id)
            XCTAssertEqual(palette.failure, .system(.red), id)
        }
    }

    func testANSIThemesUseSystemColoursInTheirAppearance() throws {
        let dark = ThemePalette(try theme("ansi"))
        XCTAssertEqual(dark.scheme, .dark)
        XCTAssertFalse(dark.paintsBackgrounds)
        XCTAssertEqual(dark.accent, .system(.green))
        XCTAssertEqual(dark.onAccent, .system(.black))
        XCTAssertEqual(dark.text, .system(.label))
        XCTAssertEqual(dark.secondaryText, .system(.secondaryLabel))
        XCTAssertEqual(dark.warning, .system(.orange))
        XCTAssertEqual(dark.violet, .system(.purple))
        XCTAssertEqual(dark.code, .system(.secondaryBackground))

        let light = ThemePalette(try theme("ansi-light"))
        XCTAssertEqual(light.scheme, .light)
        XCTAssertEqual(light.onAccent, .system(.white))
    }

    func testThemesWithTheirOwnBackgroundPaintIt() throws {
        let dracula = ThemePalette(try theme("dracula"))
        XCTAssertEqual(dracula.scheme, .dark)
        XCTAssertTrue(dracula.paintsBackgrounds)
        XCTAssertEqual(dracula.background, .rgb(RGB(hex: 0x282A36)))
        XCTAssertEqual(dracula.groupedBackground, .rgb(RGB(hex: 0x282A36)))
        XCTAssertEqual(dracula.surface, .rgb(RGB(hex: 0x282A36).mixed(with: RGB(hex: 0xF8F8F2), amount: 0.08)))
        XCTAssertEqual(dracula.surface, dracula.code)
        XCTAssertEqual(dracula.text, .rgb(RGB(hex: 0xF8F8F2)))
        XCTAssertEqual(dracula.secondaryText, .rgb(RGB(hex: 0x8692B7)))
        XCTAssertEqual(dracula.tertiaryText, .rgb(RGB(hex: 0x6A7390)))
        XCTAssertEqual(dracula.accent, .rgb(RGB(hex: 0xBD93F9)))
        XCTAssertEqual(dracula.onAccent, .rgb(RGB(hex: 0x282A36)))
        XCTAssertEqual(dracula.success, .rgb(RGB(hex: 0x50FA7B)))
        XCTAssertEqual(dracula.failure, .rgb(RGB(hex: 0xFF5555)))
        XCTAssertEqual(dracula.warning, .rgb(RGB(hex: 0xF1FA8C)))
        XCTAssertEqual(dracula.violet, .rgb(RGB(hex: 0xFF79C6)))
        XCTAssertEqual(dracula.info, dracula.accent)

        let github = ThemePalette(try theme("github-light"))
        XCTAssertEqual(github.scheme, .light)
        XCTAssertEqual(github.surface, .rgb(.white))
        guard case .rgb(let page) = github.groupedBackground else {
            return XCTFail("github-light page isn't a colour")
        }
        XCTAssertLessThan(page.luminance, 1)
        XCTAssertGreaterThan(page.luminance, 0.85)
        XCTAssertEqual(github.code, github.groupedBackground)
    }

    func testPaintedThemesKeepTextReadable() throws {
        for theme in ThemeCatalog.all {
            let palette = ThemePalette(theme)
            guard palette.paintsBackgrounds else { continue }
            func rgb(_ swatch: Swatch) throws -> RGB {
                guard case .rgb(let rgb) = swatch else {
                    return try XCTUnwrap(nil, "\(theme.id) paints with \(swatch)")
                }
                return rgb
            }
            let text = try rgb(palette.text)
            // The terminal keeps text at 7:1 on the background; rows and code stay above 4.5:1.
            XCTAssertGreaterThanOrEqual(text.contrast(with: try rgb(palette.background)), 7, theme.id)
            for surface in [palette.background, palette.surface, palette.code, palette.groupedBackground] {
                let surface = try rgb(surface)
                XCTAssertGreaterThanOrEqual(text.contrast(with: surface), 4.5, theme.id)
                for swatch in [
                    palette.secondaryText, palette.accent, palette.success, palette.failure, palette.warning,
                ] {
                    let ratio = try rgb(swatch).contrast(with: surface)
                    XCTAssertGreaterThanOrEqual(ratio, 3.5, "\(theme.id) \(swatch)")
                }
            }
            let onAccent = try rgb(palette.onAccent).contrast(with: try rgb(palette.accent))
            XCTAssertGreaterThanOrEqual(onAccent, 4.5, theme.id)
        }
    }

    func testIconNamesAndColours() throws {
        XCTAssertNil(try theme("default").iconName)
        XCTAssertEqual(try theme("gruvbox-dark").iconName, "AppIcon-gruvbox-dark")
        let names = ThemeCatalog.all.compactMap(\.iconName)
        XCTAssertEqual(names.count, ThemeCatalog.all.count - 1)
        XCTAssertEqual(Set(names).count, names.count)

        func colors(_ id: String) throws -> [String] {
            let colors = try theme(id).iconColors
            return [colors.text.hex, colors.background.hex]
        }
        // Default matches the primary icon.
        XCTAssertEqual(try colors("default"), ["00ff41", "050805"])
        XCTAssertEqual(try colors("amber"), ["ffb000", "050805"])
        XCTAssertEqual(try colors("dracula"), ["bd93f9", "282a36"])
        XCTAssertEqual(try colors("github-light"), ["0969da", "ffffff"])
        XCTAssertEqual(try colors("ansi"), ["30d158", "050805"])
        XCTAssertEqual(try colors("ansi-light"), ["34c759", "ffffff"])
    }

    /// `scripts/app-icons.sh` renders an icon set per theme into the app's asset catalog.
    func testEveryThemeHasAnAppIconSet() throws {
        let catalog = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("Difu/Assets.xcassets")
        let sets = try FileManager.default.contentsOfDirectory(atPath: catalog.path)
            .filter { $0.hasPrefix("AppIcon-") && $0.hasSuffix(".appiconset") }
            .map { String($0.dropLast(".appiconset".count)) }
        XCTAssertEqual(Set(sets), Set(ThemeCatalog.all.compactMap(\.iconName)))
        for name in sets {
            let png = catalog.appendingPathComponent("\(name).appiconset/\(name).png")
            XCTAssertTrue(FileManager.default.fileExists(atPath: png.path), name)
        }
    }
}
