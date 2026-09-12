//! ratatuiの[`Line`]を、端末に直接流せるANSIエスケープ付き文字列にする。
//! `folio render`（yaziのプレビュー欄や`less -R`向け）で使う。TUIを開かない出力経路。

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

/// 1行をANSI文字列にする（末尾の改行は含まない）。行末で装飾をリセットする。
pub fn line_to_ansi(line: &Line<'_>) -> String {
    let mut out = String::new();
    let mut current = Style::default();
    for span in &line.spans {
        let style = line.style.patch(span.style);
        if style != current {
            out.push_str("\x1b[0m");
            out.push_str(&sgr(style));
            current = style;
        }
        out.push_str(&span.content);
    }
    if current != Style::default() {
        out.push_str("\x1b[0m");
    }
    out
}

/// 装飾をSGRコードにする。既定（無装飾）なら空文字。
fn sgr(style: Style) -> String {
    let mut codes: Vec<String> = Vec::new();
    let m = style.add_modifier;
    if m.contains(Modifier::BOLD) {
        codes.push("1".into());
    }
    if m.contains(Modifier::DIM) {
        codes.push("2".into());
    }
    if m.contains(Modifier::ITALIC) {
        codes.push("3".into());
    }
    if m.contains(Modifier::UNDERLINED) {
        codes.push("4".into());
    }
    if m.contains(Modifier::CROSSED_OUT) {
        codes.push("9".into());
    }
    if let Some(fg) = style.fg
        && let Some(code) = color_code(fg, false)
    {
        codes.push(code);
    }
    if let Some(bg) = style.bg
        && let Some(code) = color_code(bg, true)
    {
        codes.push(code);
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

fn color_code(color: Color, bg: bool) -> Option<String> {
    let base = if bg { 40 } else { 30 };
    let bright = if bg { 100 } else { 90 };
    let code = match color {
        Color::Reset => return None,
        Color::Black => base.to_string(),
        Color::Red => (base + 1).to_string(),
        Color::Green => (base + 2).to_string(),
        Color::Yellow => (base + 3).to_string(),
        Color::Blue => (base + 4).to_string(),
        Color::Magenta => (base + 5).to_string(),
        Color::Cyan => (base + 6).to_string(),
        Color::Gray => (base + 7).to_string(),
        Color::DarkGray => bright.to_string(),
        Color::LightRed => (bright + 1).to_string(),
        Color::LightGreen => (bright + 2).to_string(),
        Color::LightYellow => (bright + 3).to_string(),
        Color::LightBlue => (bright + 4).to_string(),
        Color::LightMagenta => (bright + 5).to_string(),
        Color::LightCyan => (bright + 6).to_string(),
        Color::White => (bright + 7).to_string(),
        Color::Indexed(i) => format!("{};5;{i}", base + 8),
        Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", base + 8),
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    #[test]
    fn plain_line_has_no_escapes() {
        assert_eq!(line_to_ansi(&Line::from("abc")), "abc");
    }

    #[test]
    fn styled_span_is_wrapped_with_reset() {
        let line = Line::from(vec![
            Span::styled(
                "a",
                Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
            ),
            Span::raw("b"),
        ]);
        assert_eq!(line_to_ansi(&line), "\x1b[0m\x1b[1;34ma\x1b[0mb");
    }

    #[test]
    fn indexed_and_rgb_colors() {
        let line = Line::from(vec![Span::styled(
            "x",
            Style::new().fg(Color::Rgb(1, 2, 3)).bg(Color::Indexed(236)),
        )]);
        assert_eq!(
            line_to_ansi(&line),
            "\x1b[0m\x1b[38;2;1;2;3;48;5;236mx\x1b[0m"
        );
    }
}
