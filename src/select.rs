//! 描画後の行に対するマウスでの範囲選択と、コピーする文字列。中核の一部で、ターミナルには触らない。
//!
//! 位置は画面の桁（表示幅）で受け、行内の文字（char）位置に直して扱う。コピーするのは
//! 見た目どおりの本文で、余白と行末の空白は含めず、折り返しで分かれた行はつなぐ
//! （つながりは描画側が[`Flow`]に記録する）。

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use unicode_width::UnicodeWidthChar;

use crate::render::{Flow, Joint};
use crate::search;

/// 描画後の行の中の位置。`col`は行頭からの桁（表示幅）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub line: usize,
    pub col: usize,
}

/// 選んだ範囲。`anchor`は押した位置、`head`は今の位置。両端の桁を含む。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
}

impl Selection {
    fn ordered(&self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

/// ドラッグの進み具合。押して始め、動かして広げ、離した時に選択を確定する。
#[derive(Debug, Clone, Copy, Default)]
pub struct Dragger {
    selection: Option<Selection>,
    dragging: bool,
    moved: bool,
}

impl Dragger {
    pub fn begin(&mut self, at: Point) {
        *self = Self {
            selection: Some(Selection {
                anchor: at,
                head: at,
            }),
            dragging: true,
            moved: false,
        };
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    pub fn extend(&mut self, at: Point) {
        if !self.dragging {
            return;
        }
        if let Some(s) = &mut self.selection
            && s.head != at
        {
            s.head = at;
            self.moved = true;
        }
    }

    /// 離した時。動かしていれば選択を残して返し、押しただけなら選択を消す。
    pub fn finish(&mut self) -> Option<Selection> {
        if !self.dragging {
            return None;
        }
        self.dragging = false;
        if !self.moved {
            self.selection = None;
        }
        self.selection
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// 選んだ文字の見た目。
pub fn style() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

/// 画面の位置(`x`,`y`)を、`area`に`scroll`行目から描いた行の中の位置に直す。
/// 領域の上下にはみ出した位置は端の行に、左にはみ出した位置は行頭に寄せる。
pub fn point_at(area: Rect, scroll: usize, total: usize, x: u16, y: u16) -> Option<Point> {
    if total == 0 || area.height == 0 {
        return None;
    }
    let row = y.clamp(area.y, area.bottom() - 1) - area.y;
    Some(Point {
        line: (scroll + row as usize).min(total - 1),
        col: x.saturating_sub(area.x) as usize,
    })
}

/// ドラッグが領域の上下にはみ出した時のスクロール量（上なら-1、下なら+1）。
pub fn edge_scroll(area: Rect, y: u16) -> isize {
    if y < area.y {
        -1
    } else if y >= area.bottom() {
        1
    } else {
        0
    }
}

/// 行`i`のうち選択に入る文字（char）の範囲`[from, to)`。
/// 行末の空白と、折り返しの続きの行の行頭の飾り（字下げ・引用の縦線）は含めない。
pub fn chars_in_line(
    line: &Line<'_>,
    flow: &[Flow],
    sel: Selection,
    i: usize,
) -> Option<(usize, usize)> {
    let (start, end) = sel.ordered();
    if i < start.line || i > end.line {
        return None;
    }
    let chars: Vec<char> = search::plain(line).chars().collect();
    let content_end = chars
        .iter()
        .rposition(|c| !c.is_whitespace())
        .map_or(0, |p| p + 1);
    let mut from = if i == start.line {
        char_at(&chars, start.col)
    } else {
        0
    };
    if i > 0 && joins_next(flow, i - 1) {
        from = from.max(flow.get(i).map_or(0, |f| f.prefix));
    }
    let to = if i == end.line {
        (char_at(&chars, end.col) + 1).min(chars.len())
    } else {
        chars.len()
    }
    .min(content_end);
    (from < to).then_some((from, to))
}

/// 選んだ範囲の文字列。折り返しで分かれた行はつなぎ、原文で分かれた行は改行で区切る。
pub fn text(lines: &[Line<'_>], flow: &[Flow], sel: Selection) -> String {
    let (start, end) = sel.ordered();
    let mut out = String::new();
    if lines.is_empty() {
        return out;
    }
    let last = end.line.min(lines.len() - 1);
    for (i, line) in lines.iter().enumerate().take(last + 1).skip(start.line) {
        if i > start.line {
            out.push_str(match flow.get(i - 1).map(|f| f.joint) {
                Some(Joint::Soft) => "",
                Some(Joint::SoftSpace) => " ",
                _ => "\n",
            });
        }
        if let Some((from, to)) = chars_in_line(line, flow, sel, i) {
            out.extend(search::plain(line).chars().skip(from).take(to - from));
        }
    }
    out
}

/// 端末にクリップボードへ書かせる制御列（OSC 52）。Windows Terminalなどが対応する。
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn joins_next(flow: &[Flow], i: usize) -> bool {
    flow.get(i).is_some_and(|f| f.joint != Joint::Hard)
}

/// 桁`col`にかかる文字の添字。行より右なら文字数。
fn char_at(chars: &[char], col: usize) -> usize {
    let mut w = 0;
    for (k, c) in chars.iter().enumerate() {
        w += c.width().unwrap_or(0);
        if col < w {
            return k;
        }
    }
    chars.len()
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * k)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::parse;
    use crate::render::{Options, Theme, render_with};

    fn rendered(md: &str, width: u16) -> crate::render::Rendered {
        render_with(
            &parse(md),
            width,
            &Theme::default(),
            None,
            &Options::default(),
        )
    }

    fn sel(a: (usize, usize), b: (usize, usize)) -> Selection {
        Selection {
            anchor: Point {
                line: a.0,
                col: a.1,
            },
            head: Point {
                line: b.0,
                col: b.1,
            },
        }
    }

    #[test]
    fn joins_wrapped_lines_and_keeps_paragraph_breaks() {
        let r = rendered("aaa bbb ccc ddd\n\nあいうえおかきくけこ", 8);
        // aaa bbb / ccc ddd / 空 / あいうえ / おかきく / けこ
        let all = sel((0, 0), (r.lines.len() - 1, 99));
        assert_eq!(
            text(&r.lines, &r.flow, all),
            "aaa bbb ccc ddd\n\nあいうえおかきくけこ"
        );
    }

    #[test]
    fn drops_prefix_of_continuation_and_trailing_padding() {
        let r = rendered("- one two three four\n\n```\nabc\n```", 12);
        // • one two /   three four / 空 / abc（背景の空白で埋まる）
        let all = sel((0, 0), (3, 99));
        assert_eq!(text(&r.lines, &r.flow, all), "• one two three four\n\nabc");
    }

    #[test]
    fn partial_selection_uses_display_columns_and_any_direction() {
        let r = rendered("あいうえお", 20);
        // 桁2〜5は「い」「う」。逆向きのドラッグでも同じ
        assert_eq!(text(&r.lines, &r.flow, sel((0, 2), (0, 5))), "いう");
        assert_eq!(text(&r.lines, &r.flow, sel((0, 5), (0, 2))), "いう");
        // 行より右を押しても行末まで
        assert_eq!(
            chars_in_line(&r.lines[0], &r.flow, sel((0, 6), (0, 40)), 0),
            Some((3, 5))
        );
    }

    #[test]
    fn blank_line_has_nothing_to_highlight() {
        let r = rendered("a\n\nb", 20);
        assert_eq!(
            chars_in_line(&r.lines[1], &r.flow, sel((0, 0), (2, 0)), 1),
            None
        );
    }

    #[test]
    fn point_is_clamped_into_area() {
        let area = Rect::new(10, 2, 30, 5);
        let p = |x, y| point_at(area, 100, 103, x, y).unwrap();
        assert_eq!(p(12, 3), Point { line: 101, col: 2 });
        // 上下にはみ出たら端の行、行数を超えたら最後の行、左にはみ出たら行頭
        assert_eq!(p(12, 0).line, 100);
        assert_eq!(p(12, 20).line, 102);
        assert_eq!(p(0, 2).col, 0);
        assert_eq!(point_at(area, 0, 0, 12, 3), None);
        assert_eq!(
            (
                edge_scroll(area, 1),
                edge_scroll(area, 4),
                edge_scroll(area, 7)
            ),
            (-1, 0, 1)
        );
    }

    #[test]
    fn click_without_move_selects_nothing() {
        let mut d = Dragger::default();
        d.begin(Point { line: 1, col: 1 });
        assert_eq!(d.finish(), None);
        d.begin(Point { line: 1, col: 1 });
        d.extend(Point { line: 2, col: 3 });
        assert!(d.finish().is_some());
        assert!(!d.is_dragging() && d.selection().is_some());
    }

    #[test]
    fn osc52_encodes_utf8_as_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(osc52("日本"), "\x1b]52;c;5pel5pys\x07");
    }
}
