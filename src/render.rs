//! ブロック構造を、指定幅に折り返した装飾付きの行（ratatuiの[`Line`]）へ変換する。
//!
//! ターミナルには触らない。幅だけを受け取って行の列を返すので、ビューアーでも
//! 将来のファイラーのプレビュー欄でも同じ関数を呼べる。

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::document::{Align, Block, Inline, InlineStyle, ListItem};

/// 配色。P1ではハードコード（設定ファイルはバックログ）。
#[derive(Debug, Clone)]
pub struct Theme {
    pub heading: [Style; 6],
    pub heading_rule: [Option<char>; 6],
    pub code_block: Style,
    pub inline_code: Style,
    pub link: Style,
    pub quote_bar: Style,
    pub quote_text: Style,
    pub bullet: Style,
    pub rule: Style,
    pub table_border: Style,
    pub table_header: Style,
}

impl Default for Theme {
    fn default() -> Self {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        Self {
            heading: [
                bold.fg(Color::Yellow),
                bold.fg(Color::Cyan),
                bold.fg(Color::Green),
                bold.fg(Color::Magenta),
                bold,
                bold.add_modifier(Modifier::ITALIC),
            ],
            heading_rule: [Some('━'), Some('─'), None, None, None, None],
            code_block: Style::new().bg(Color::Indexed(236)),
            inline_code: Style::new().fg(Color::Indexed(216)).bg(Color::Indexed(236)),
            link: Style::new()
                .fg(Color::Blue)
                .add_modifier(Modifier::UNDERLINED),
            quote_bar: Style::new().fg(Color::DarkGray),
            quote_text: Style::new().fg(Color::Indexed(250)),
            bullet: Style::new().fg(Color::Cyan),
            rule: Style::new().fg(Color::DarkGray),
            table_border: Style::new().fg(Color::DarkGray),
            table_header: bold,
        }
    }
}

/// 折り返し幅がこれより狭い時は、この幅で描いてはみ出させる（0幅で無限ループしないため）。
const MIN_WIDTH: usize = 8;

/// 文書を`width`桁に収めた行の列にする。例外は2つ: 表は折り返さずはみ出すことがある。
/// 行頭禁則の句読点は前の行にぶら下がり、最大2桁はみ出す。
pub fn render(blocks: &[Block], width: u16) -> Vec<Line<'static>> {
    render_with(blocks, width, &Theme::default())
}

pub fn render_with(blocks: &[Block], width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let mut renderer = Renderer {
        width: (width as usize).max(MIN_WIDTH),
        theme,
        lines: Vec::new(),
    };
    renderer.blocks(blocks, &Prefix::default(), false);
    while renderer.lines.last().is_some_and(is_blank) {
        renderer.lines.pop();
    }
    renderer.lines
}

fn is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|s| s.content.trim().is_empty())
}

/// 行頭に付ける飾り。リストの記号や引用の縦線がここに積まれる。
/// `first`はブロックの1行目用（記号入り）、`rest`は2行目以降用（同じ幅の空白）。
#[derive(Debug, Clone, Default)]
struct Prefix {
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
}

impl Prefix {
    fn child(&self, first: Span<'static>, rest: Span<'static>) -> Prefix {
        let mut p = self.clone();
        p.first.push(first);
        p.rest.push(rest);
        p
    }

    /// 1行目の飾りを消費した後の状態（以降のブロックは`rest`で始まる）。
    fn continued(&self) -> Prefix {
        Prefix {
            first: self.rest.clone(),
            rest: self.rest.clone(),
        }
    }

    fn width(&self) -> usize {
        self.rest.iter().map(|s| s.content.width()).sum()
    }
}

struct Renderer<'t> {
    width: usize,
    theme: &'t Theme,
    lines: Vec<Line<'static>>,
}

impl Renderer<'_> {
    fn avail(&self, prefix: &Prefix) -> usize {
        self.width.saturating_sub(prefix.width()).max(MIN_WIDTH)
    }

    fn emit(&mut self, prefix: &[Span<'static>], content: Vec<Span<'static>>) {
        let mut spans = prefix.to_vec();
        spans.extend(content);
        self.lines.push(Line::from(spans));
    }

    fn blank(&mut self, prefix: &Prefix) {
        self.emit(&prefix.rest, Vec::new());
    }

    /// ブロック列を描く。`in_item`はリスト項目の直下かどうか（段落と入れ子リストの間を詰める）。
    fn blocks(&mut self, blocks: &[Block], prefix: &Prefix, in_item: bool) {
        let mut prefix = prefix.clone();
        for (i, block) in blocks.iter().enumerate() {
            if i > 0 {
                let tight = in_item
                    && matches!(blocks[i - 1], Block::Paragraph(_))
                    && matches!(block, Block::List { .. });
                if !tight {
                    self.blank(&prefix);
                }
            }
            self.block(block, &prefix);
            prefix = prefix.continued();
        }
    }

    fn block(&mut self, block: &Block, prefix: &Prefix) {
        match block {
            Block::Heading { level, inlines } => self.heading(*level, inlines, prefix),
            Block::Paragraph(inlines) => self.paragraph(inlines, prefix, Style::default()),
            Block::List { start, items } => self.list(*start, items, prefix),
            Block::CodeBlock { code, .. } => self.code_block(code, prefix),
            Block::BlockQuote(inner) => {
                let bar = Span::styled("▌ ", self.theme.quote_bar);
                let child = prefix.child(bar.clone(), bar);
                // 引用の中身は少し落ち着いた色に。ここでは中身の段落だけ色を変える
                let inner: Vec<Block> = inner.clone();
                self.quote_blocks(&inner, &child);
            }
            Block::Rule => {
                let avail = self.avail(prefix);
                self.emit(
                    &prefix.first,
                    vec![Span::styled("─".repeat(avail), self.theme.rule)],
                );
            }
            Block::Table {
                aligns,
                header,
                rows,
            } => self.table(aligns, header, rows, prefix),
        }
    }

    fn quote_blocks(&mut self, blocks: &[Block], prefix: &Prefix) {
        let mut prefix = prefix.clone();
        for (i, block) in blocks.iter().enumerate() {
            if i > 0 {
                self.blank(&prefix);
            }
            match block {
                Block::Paragraph(inlines) => {
                    self.paragraph(inlines, &prefix, self.theme.quote_text)
                }
                other => self.block(other, &prefix),
            }
            prefix = prefix.continued();
        }
    }

    fn heading(&mut self, level: u8, inlines: &[Inline], prefix: &Prefix) {
        let idx = (level.clamp(1, 6) - 1) as usize;
        let style = self.theme.heading[idx];
        let avail = self.avail(prefix);
        let wrapped = wrap_inlines(inlines, avail, style, self.theme);
        let mut first = true;
        for spans in wrapped {
            let p = if first { &prefix.first } else { &prefix.rest };
            self.emit(p, spans);
            first = false;
        }
        if let Some(ch) = self.theme.heading_rule[idx] {
            self.emit(
                &prefix.rest,
                vec![Span::styled(ch.to_string().repeat(avail), style)],
            );
        }
    }

    fn paragraph(&mut self, inlines: &[Inline], prefix: &Prefix, base: Style) {
        let avail = self.avail(prefix);
        let mut first = true;
        for spans in wrap_inlines(inlines, avail, base, self.theme) {
            let p = if first { &prefix.first } else { &prefix.rest };
            self.emit(p, spans);
            first = false;
        }
    }

    fn list(&mut self, start: Option<u64>, items: &[ListItem], prefix: &Prefix) {
        // 入れ子の深さは、既に積まれている記号の数で決める（記号を変えて段を見せる）
        let depth = prefix
            .rest
            .iter()
            .filter(|s| s.content.chars().all(|c| c == ' ') && !s.content.is_empty())
            .count();
        let bullets = ['•', '◦', '▪'];
        // 番号付きは桁を揃える
        let number_width = start
            .map(|s| (s + items.len() as u64).saturating_sub(1).to_string().len())
            .unwrap_or(0);
        for (i, item) in items.iter().enumerate() {
            let marker = match (start, item.task) {
                (_, Some(true)) => "☑ ".to_string(),
                (_, Some(false)) => "☐ ".to_string(),
                (Some(s), None) => format!("{:>width$}. ", s + i as u64, width = number_width),
                (None, None) => format!("{} ", bullets[depth % bullets.len()]),
            };
            let pad = " ".repeat(marker.width());
            let child = prefix.child(Span::styled(marker, self.theme.bullet), Span::raw(pad));
            self.blocks(&item.blocks, &child, true);
        }
    }

    fn code_block(&mut self, code: &str, prefix: &Prefix) {
        let avail = self.avail(prefix);
        let style = self.theme.code_block;
        let lines: Vec<&str> = if code.is_empty() {
            vec![""]
        } else {
            code.lines().collect()
        };
        let mut first = true;
        for raw in lines {
            let text = expand_tabs(raw);
            let w = text.width();
            let padded = if w < avail {
                format!("{text}{}", " ".repeat(avail - w))
            } else {
                text
            };
            let p = if first { &prefix.first } else { &prefix.rest };
            self.emit(p, vec![Span::styled(padded, style)]);
            first = false;
        }
    }

    fn table(
        &mut self,
        aligns: &[Align],
        header: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        prefix: &Prefix,
    ) {
        let ncols = header
            .len()
            .max(rows.iter().map(Vec::len).max().unwrap_or(0));
        if ncols == 0 {
            return;
        }
        let cell_width = |cell: &[Inline]| cell.iter().map(|i| i.text.width()).sum::<usize>();
        let mut widths = vec![0usize; ncols];
        for row in std::iter::once(header).chain(rows.iter().map(Vec::as_slice)) {
            for (c, cell) in row.iter().enumerate() {
                widths[c] = widths[c].max(cell_width(cell));
            }
        }
        let border = self.theme.table_border;
        let sep = Span::styled(" │ ", border);
        let empty: Vec<Inline> = Vec::new();

        let row_spans = |row: &[Vec<Inline>], base: Style, theme: &Theme| -> Vec<Span<'static>> {
            let mut spans = Vec::new();
            for (c, col_width) in widths.iter().enumerate() {
                if c > 0 {
                    spans.push(sep.clone());
                }
                let cell = row.get(c).unwrap_or(&empty);
                let w = cell_width(cell);
                let pad = col_width.saturating_sub(w);
                let (left, right) = match aligns.get(c).copied().unwrap_or(Align::Left) {
                    Align::Left => (0, pad),
                    Align::Right => (pad, 0),
                    Align::Center => (pad / 2, pad - pad / 2),
                };
                if left > 0 {
                    spans.push(Span::raw(" ".repeat(left)));
                }
                for inline in cell {
                    if inline.is_line_break() {
                        continue;
                    }
                    spans.push(Span::styled(
                        inline.text.clone(),
                        style_of(&inline.style, base, theme),
                    ));
                }
                if right > 0 {
                    spans.push(Span::raw(" ".repeat(right)));
                }
            }
            spans
        };

        let head = row_spans(header, self.theme.table_header, self.theme);
        self.emit(&prefix.first, head);
        let rule: Vec<Span<'static>> = widths
            .iter()
            .enumerate()
            .flat_map(|(c, w)| {
                let mut v = Vec::new();
                if c > 0 {
                    v.push(Span::styled("─┼─", border));
                }
                v.push(Span::styled("─".repeat(*w), border));
                v
            })
            .collect();
        self.emit(&prefix.rest, rule);
        for row in rows {
            let spans = row_spans(row, Style::default(), self.theme);
            self.emit(&prefix.rest, spans);
        }
    }
}

fn expand_tabs(s: &str) -> String {
    s.replace('\t', "    ")
}

fn style_of(inline: &InlineStyle, base: Style, theme: &Theme) -> Style {
    if inline.code {
        return theme.inline_code;
    }
    let mut style = base;
    if inline.link.is_some() {
        style = style.patch(theme.link);
    }
    if inline.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if inline.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if inline.strike {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    style
}

/// 折り返しの単位。空白の並び、1語（ASCIIの連なり）、または全角1文字。
#[derive(Debug, Clone)]
enum Token {
    Space(usize, Style),
    Word(String, usize, Style),
    Break,
}

fn tokenize(inlines: &[Inline], base: Style, theme: &Theme) -> Vec<Token> {
    let mut tokens = Vec::new();
    for inline in inlines {
        if inline.is_line_break() {
            tokens.push(Token::Break);
            continue;
        }
        let style = style_of(&inline.style, base, theme);
        let mut word = String::new();
        let mut word_w = 0;
        let flush = |word: &mut String, word_w: &mut usize, tokens: &mut Vec<Token>| {
            if !word.is_empty() {
                tokens.push(Token::Word(std::mem::take(word), *word_w, style));
                *word_w = 0;
            }
        };
        let text = expand_tabs(&inline.text);
        for ch in text.chars() {
            let w = ch.width().unwrap_or(0);
            if ch == ' ' {
                flush(&mut word, &mut word_w, &mut tokens);
                match tokens.last_mut() {
                    Some(Token::Space(n, s)) if *s == style => *n += 1,
                    _ => tokens.push(Token::Space(1, style)),
                }
            } else if w >= 2 || is_cjk(ch) {
                // 全角・CJKはどこでも折り返せるので1文字1語
                flush(&mut word, &mut word_w, &mut tokens);
                tokens.push(Token::Word(ch.to_string(), w, style));
            } else if ch == '\n' {
                flush(&mut word, &mut word_w, &mut tokens);
                tokens.push(Token::Break);
            } else {
                word.push(ch);
                word_w += w;
            }
        }
        flush(&mut word, &mut word_w, &mut tokens);
    }
    tokens
}

/// 行頭に置かない文字（句読点・閉じ括弧・長音など）。1文字の語だけを対象にする。
fn is_no_line_start(text: &str) -> bool {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => "、。，．,.:;!?）」』】〕〉》〙〟｝］ー〜…‥ぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮヵヶ)]}".contains(ch),
        _ => false,
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3000..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FA1F)
}

/// インライン列を`avail`桁で折り返し、行ごとのスパン列にする。
fn wrap_inlines(
    inlines: &[Inline],
    avail: usize,
    base: Style,
    theme: &Theme,
) -> Vec<Vec<Span<'static>>> {
    let avail = avail.max(1);
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    let mut pending_space: Option<(usize, Style)> = None;

    fn push_span(cur: &mut Vec<Span<'static>>, text: &str, style: Style) {
        match cur.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push_str(text),
            _ => cur.push(Span::styled(text.to_string(), style)),
        }
    }

    for token in tokenize(inlines, base, theme) {
        match token {
            Token::Break => {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
                pending_space = None;
            }
            Token::Space(n, style) => {
                if cur_w > 0 {
                    pending_space = Some((n, style));
                }
            }
            Token::Word(text, w, style) => {
                let space_w = pending_space.map(|(n, _)| n).unwrap_or(0);
                // 行頭禁則の文字（句読点・閉じ括弧）は幅を超えても前の行にぶら下げる
                let hang = cur_w > 0 && space_w == 0 && is_no_line_start(&text);
                if cur_w > 0 && cur_w + space_w + w > avail && !hang {
                    lines.push(std::mem::take(&mut cur));
                    cur_w = 0;
                    pending_space = None;
                } else if let Some((n, s)) = pending_space.take() {
                    push_span(&mut cur, &" ".repeat(n), s);
                    cur_w += n;
                }
                if w > avail {
                    // 1語が幅を超える（長いURLなど）。文字単位で割る
                    let mut chunk = String::new();
                    let mut chunk_w = 0;
                    for ch in text.chars() {
                        let cw = ch.width().unwrap_or(0);
                        if cur_w + chunk_w + cw > avail {
                            push_span(&mut cur, &chunk, style);
                            lines.push(std::mem::take(&mut cur));
                            cur_w = 0;
                            chunk.clear();
                            chunk_w = 0;
                        }
                        chunk.push(ch);
                        chunk_w += cw;
                    }
                    push_span(&mut cur, &chunk, style);
                    cur_w += chunk_w;
                } else {
                    push_span(&mut cur, &text, style);
                    cur_w += w;
                }
            }
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::parse;

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn widths(lines: &[Line<'_>]) -> Vec<usize> {
        lines.iter().map(|l| l.width()).collect()
    }

    #[test]
    fn wraps_ascii_at_word_boundary() {
        let lines = render(&parse("aaa bbb ccc ddd"), 8);
        assert_eq!(plain(&lines), vec!["aaa bbb", "ccc ddd"]);
    }

    #[test]
    fn wraps_japanese_by_display_width() {
        // 全角は2桁。幅10なら5文字で折り返す
        let lines = render(&parse("あいうえおかきくけこ"), 10);
        assert_eq!(plain(&lines), vec!["あいうえお", "かきくけこ"]);
        assert!(widths(&lines).iter().all(|w| *w <= 10));
    }

    #[test]
    fn punctuation_hangs_instead_of_starting_a_line() {
        // 「、」は行頭に来ない。幅を1桁超えて前の行にぶら下がる
        let lines = render(&parse("あいうえお、かき"), 10);
        assert_eq!(plain(&lines), vec!["あいうえお、", "かき"]);
    }

    #[test]
    fn long_word_is_split_by_char() {
        let lines = render(&parse("https://example.com/aaaaaaaaaaaaaaaaaaaaaa"), 10);
        assert!(widths(&lines).iter().all(|w| *w <= 10));
        assert_eq!(
            plain(&lines).concat(),
            "https://example.com/aaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn nested_list_keeps_indent_on_continuation() {
        let lines = render(&parse("- one two three four\n  - nested item"), 12);
        let text = plain(&lines);
        assert_eq!(text[0], "• one two");
        assert_eq!(text[1], "  three four");
        assert_eq!(text[2], "  ◦ nested");
    }

    #[test]
    fn heading_gets_rule_and_paragraph_gets_blank_between() {
        let lines = render(&parse("# T\n\np\n"), 20);
        let text = plain(&lines);
        assert_eq!(text[0], "T");
        assert_eq!(text[1], "━".repeat(20));
        assert_eq!(text[2], "");
        assert_eq!(text[3], "p");
    }

    #[test]
    fn code_block_is_not_wrapped_and_is_padded() {
        let lines = render(&parse("```\nabc\n\tx\n```"), 10);
        let text = plain(&lines);
        assert_eq!(text, vec!["abc       ", "    x     "]);
    }

    #[test]
    fn quote_bar_continues_on_wrapped_lines() {
        let lines = render(&parse("> aaa bbb ccc"), 9);
        assert_eq!(plain(&lines), vec!["▌ aaa bbb", "▌ ccc"]);
    }

    #[test]
    fn table_aligns_columns() {
        let lines = render(&parse("| a | bb |\n|---|---:|\n| ccc | d |"), 40);
        assert_eq!(plain(&lines), vec!["a   │ bb", "────┼───", "ccc │  d"]);
    }

    #[test]
    fn ordered_list_pads_numbers() {
        let md = (1..=10)
            .map(|i| format!("{i}. x"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = render(&parse(&md), 20);
        let text = plain(&lines);
        assert_eq!(text[0], " 1. x");
        assert_eq!(text[9], "10. x");
    }
}
