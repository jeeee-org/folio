//! コードブロックの言語別シンタックスハイライト（syntect）。
//! 中核の一部で、ターミナルには触らない。配色はratatuiの[`Style`]で返す。

use ratatui::style::{Color, Modifier, Style};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// 文法と配色を読み込んだハイライター。読み込みに数百msかかるので、起動時に1度だけ作る。
pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

/// 既定の配色名。ビューアーのコードブロック背景（暗いグレー）に合う暗色系。
pub const DEFAULT_THEME: &str = "base16-ocean.dark";

impl Highlighter {
    pub fn new() -> Self {
        Self::with_theme(DEFAULT_THEME)
    }

    pub fn with_theme(name: &str) -> Self {
        let mut themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .remove(name)
            .or_else(|| themes.themes.remove(DEFAULT_THEME))
            .expect("syntectの既定テーマが読めない");
        Self {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            theme,
        }
    }

    /// その言語名・拡張子を知っているか。
    pub fn supports(&self, token: &str) -> bool {
        self.syntaxes.find_syntax_by_token(token).is_some()
    }

    /// 言語名を解決できたら、行ごとの装飾付き断片を返す。解決できなければ`None`。
    /// 言語名は`rust`のほかに`rs`のような拡張子や`Rust`のような表記も受け付ける。
    pub fn highlight(&self, lang: &str, code: &str) -> Option<Vec<Vec<(Style, String)>>> {
        let syntax = self.syntaxes.find_syntax_by_token(lang)?;
        let mut hl = HighlightLines::new(syntax, &self.theme);
        let mut lines = Vec::new();
        for line in LinesWithEndings::from(code) {
            let segments = hl.highlight_line(line, &self.syntaxes).ok()?;
            let spans = segments
                .into_iter()
                .map(|(style, text)| (convert(style), text.trim_end_matches('\n').to_string()))
                .filter(|(_, text)| !text.is_empty())
                .collect();
            lines.push(spans);
        }
        Some(lines)
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

/// syntectの装飾をratatuiの装飾へ。背景はコードブロック側で付けるので前景と字体だけ。
fn convert(style: syntect::highlighting::Style) -> Style {
    let fg = style.foreground;
    let mut out = Style::new().fg(Color::Rgb(fg.r, fg.g, fg.b));
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_language_is_split_into_styled_segments() {
        let hl = Highlighter::new();
        let lines = hl.highlight("rust", "fn main() {}\nlet x = 1;\n").unwrap();
        assert_eq!(lines.len(), 2);
        // キーワードとそれ以外で色が分かれる
        assert!(lines[0].len() >= 2);
        let text: String = lines[1].iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(text, "let x = 1;");
    }

    #[test]
    fn extension_token_and_unknown_language() {
        let hl = Highlighter::new();
        assert!(hl.highlight("rs", "fn f() {}").is_some());
        assert!(hl.highlight("no-such-language", "x").is_none());
    }
}
