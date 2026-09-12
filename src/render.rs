//! ブロック構造を、指定幅に折り返した装飾付きの行（ratatuiの[`Line`]）へ変換する。
//!
//! ターミナルには触らない。幅だけを受け取って行の列を返すので、ビューアーでも
//! 将来のファイラーのプレビュー欄でも同じ関数を呼べる。

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::document::{Align, Block, Inline, InlineStyle, ListItem};
use crate::highlight::Highlighter;

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
    /// `show_urls`で出すURLの色
    pub url: Style,
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
            url: Style::new().fg(Color::DarkGray),
        }
    }
}

/// 折り返し幅がこれより狭い時は、この幅で描いてはみ出させる（0幅で無限ループしないため）。
const MIN_WIDTH: usize = 8;

/// 描画結果。行の列と、目次やジャンプに使う見出しの位置。
#[derive(Debug, Clone, Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub headings: Vec<Heading>,
}

/// 描画後の見出し1つ。`line`は`Rendered::lines`の添字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub line: usize,
}

/// 文書を`width`桁に収めた行の列にする。例外は2つ: 表は折り返さずはみ出すことがある。
/// 行頭禁則の句読点は前の行にぶら下がり、最大2桁はみ出す。
pub fn render(blocks: &[Block], width: u16) -> Vec<Line<'static>> {
    render_with(blocks, width, &Theme::default(), None, &Options::default()).lines
}

/// 描き方の切り替え。配色（`Theme`）とは別に、表示の有無を持つ。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// リンクの後ろにURLを薄く出す
    pub show_urls: bool,
}

/// 配色とハイライターを指定して描く。`highlighter`が`None`ならコードは単色。
pub fn render_with(
    blocks: &[Block],
    width: u16,
    theme: &Theme,
    highlighter: Option<&Highlighter>,
    options: &Options,
) -> Rendered {
    let mut renderer = Renderer {
        width: (width as usize).max(MIN_WIDTH),
        theme,
        highlighter,
        options: *options,
        lines: Vec::new(),
        headings: Vec::new(),
    };
    renderer.blocks(blocks, &Prefix::default(), false);
    while renderer.lines.last().is_some_and(is_blank) {
        renderer.lines.pop();
    }
    Rendered {
        lines: renderer.lines,
        headings: renderer.headings,
    }
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
    highlighter: Option<&'t Highlighter>,
    options: Options,
    lines: Vec<Line<'static>>,
    headings: Vec<Heading>,
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
            Block::CodeBlock { lang, code } => self.code_block(lang.as_deref(), code, prefix),
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
        self.headings.push(Heading {
            level,
            text: inlines
                .iter()
                .filter(|i| !i.is_line_break())
                .map(|i| i.text.as_str())
                .collect(),
            line: self.lines.len(),
        });
        let idx = (level.clamp(1, 6) - 1) as usize;
        let style = self.theme.heading[idx];
        let avail = self.avail(prefix);
        let wrapped = wrap_inlines_opts(
            inlines,
            avail,
            style,
            self.theme,
            true,
            self.options.show_urls,
        );
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
        for spans in wrap_inlines_opts(
            inlines,
            avail,
            base,
            self.theme,
            true,
            self.options.show_urls,
        ) {
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

    fn code_block(&mut self, lang: Option<&str>, code: &str, prefix: &Prefix) {
        let avail = self.avail(prefix);
        let bg = self.theme.code_block;
        // 言語が分かりハイライターがあれば色分けし、無ければ単色。どちらも背景で幅いっぱいに塗る
        let highlighted = lang.and_then(|l| self.highlighter.and_then(|h| h.highlight(l, code)));
        let lines: Vec<Vec<(Style, String)>> = match highlighted {
            Some(lines) if !lines.is_empty() => lines,
            _ => {
                let raw: Vec<&str> = if code.is_empty() {
                    vec![""]
                } else {
                    code.lines().collect()
                };
                raw.into_iter()
                    .map(|l| vec![(Style::new(), l.to_string())])
                    .collect()
            }
        };
        let mut first = true;
        for segments in lines {
            let mut spans = Vec::new();
            let mut w = 0;
            for (style, text) in segments {
                let text = expand_tabs(&text);
                w += text.width();
                spans.push(Span::styled(text, bg.patch(style)));
            }
            if w < avail {
                spans.push(Span::styled(" ".repeat(avail - w), bg));
            }
            let p = if first { &prefix.first } else { &prefix.rest };
            self.emit(p, spans);
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
        let mut natural = vec![0usize; ncols];
        for row in std::iter::once(header).chain(rows.iter().map(Vec::as_slice)) {
            for (c, cell) in row.iter().enumerate() {
                natural[c] = natural[c].max(cell_width(cell));
            }
        }
        let widths = fit_columns(
            &natural,
            self.avail(prefix).saturating_sub(SEP_WIDTH * (ncols - 1)),
        );
        let border = self.theme.table_border;
        let empty: Vec<Inline> = Vec::new();

        // 1行分のセル群を、セル内折り返しを含めて何行かの描画行にする（emitは呼び出し側）
        let theme = self.theme;
        let render_row = |row: &[Vec<Inline>], base: Style| -> Vec<Vec<Span<'static>>> {
            let cells: Vec<Vec<Vec<Span<'static>>>> = (0..ncols)
                .map(|c| {
                    let cell = row.get(c).unwrap_or(&empty);
                    wrap_inlines_opts(cell, widths[c], base, theme, false, false)
                })
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
            (0..height)
                .map(|k| {
                    let mut spans = Vec::new();
                    for c in 0..ncols {
                        if c > 0 {
                            spans.push(Span::styled(" │ ", border));
                        }
                        let line = cells[c].get(k).cloned().unwrap_or_default();
                        let w: usize = line.iter().map(|sp| sp.content.width()).sum();
                        let pad = widths[c].saturating_sub(w);
                        let (left, right) = match aligns.get(c).copied().unwrap_or(Align::Left) {
                            Align::Left => (0, pad),
                            Align::Right => (pad, 0),
                            Align::Center => (pad / 2, pad - pad / 2),
                        };
                        if left > 0 {
                            spans.push(Span::raw(" ".repeat(left)));
                        }
                        spans.extend(line);
                        if right > 0 {
                            spans.push(Span::raw(" ".repeat(right)));
                        }
                    }
                    spans
                })
                .collect()
        };

        for (k, spans) in render_row(header, self.theme.table_header)
            .into_iter()
            .enumerate()
        {
            let p = if k == 0 { &prefix.first } else { &prefix.rest };
            self.emit(p, spans);
        }
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
            for spans in render_row(row, Style::default()) {
                self.emit(&prefix.rest, spans);
            }
        }
    }
}

/// 表の列の区切り" │ "の幅。
const SEP_WIDTH: usize = 3;
/// 折り返す時に各列へ最低限残す幅。
const MIN_COL_WIDTH: usize = 4;
/// この幅以下の列は「短い列」として、折り返さず自然幅のまま確保する。
const SHORT_COL_WIDTH: usize = 10;

/// 列の自然幅（内容の最大幅）を`avail`に収める。収まるならそのまま。
/// 溢れるなら、短い列は自然幅のまま確保し、残りを広い列に自然幅の比で配分する
/// （各列に最小幅を保証）。短い列だけでも入らなければ全列を比例配分に落とす。
fn fit_columns(natural: &[usize], avail: usize) -> Vec<usize> {
    let n = natural.len();
    let total: usize = natural.iter().sum();
    if total <= avail || n == 0 {
        return natural.to_vec();
    }
    if avail < n * MIN_COL_WIDTH {
        // 最小幅すら入らない。均等にして、はみ出しは諦める
        return vec![(avail / n).max(MIN_COL_WIDTH); n];
    }
    let short_total: usize = natural.iter().filter(|&&w| w <= SHORT_COL_WIDTH).sum();
    let wide_count = natural.iter().filter(|&&w| w > SHORT_COL_WIDTH).count();
    let keep_short = wide_count > 0 && short_total + wide_count * MIN_COL_WIDTH <= avail;
    let (fixed, pool): (Vec<Option<usize>>, usize) = if keep_short {
        (
            natural
                .iter()
                .map(|&w| (w <= SHORT_COL_WIDTH).then_some(w))
                .collect(),
            avail - short_total,
        )
    } else {
        (vec![None; n], avail)
    };
    let flex_total: usize = (0..n)
        .filter(|&i| fixed[i].is_none())
        .map(|i| natural[i])
        .sum();
    let mut widths: Vec<usize> = (0..n)
        .map(|i| match fixed[i] {
            Some(w) => w,
            None => (pool * natural[i] / flex_total).max(MIN_COL_WIDTH),
        })
        .collect();
    // 切り捨て・最小幅の補正でずれた分を、広い列の間で調整する
    while widths.iter().sum::<usize>() > avail {
        let Some(i) = (0..n)
            .filter(|&i| fixed[i].is_none() && widths[i] > MIN_COL_WIDTH)
            .max_by_key(|&i| widths[i])
        else {
            break;
        };
        widths[i] -= 1;
    }
    loop {
        if widths.iter().sum::<usize>() >= avail {
            break;
        }
        let Some(i) = (0..n)
            .filter(|&i| fixed[i].is_none() && widths[i] < natural[i])
            .max_by_key(|&i| natural[i] - widths[i])
        else {
            break;
        };
        widths[i] += 1;
    }
    widths
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

fn tokenize(inlines: &[Inline], base: Style, theme: &Theme, show_urls: bool) -> Vec<Token> {
    let mut tokens = Vec::new();
    for (idx, inline) in inlines.iter().enumerate() {
        if inline.is_line_break() {
            tokens.push(Token::Break);
            continue;
        }
        // リンクの並び（太字などで断片が分かれていても同じURL）の最後にURLを添える
        let url_after = match (&inline.style.link, show_urls) {
            (Some(url), true)
                if inlines.get(idx + 1).and_then(|n| n.style.link.as_ref()) != Some(url) =>
            {
                Some(url.clone())
            }
            _ => None,
        };
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
        if let Some(url) = url_after {
            let w = url.width();
            tokens.push(Token::Space(1, theme.url));
            tokens.push(Token::Word(format!("({url})"), w + 2, theme.url));
        }
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
/// `allow_hang`が偽なら行頭禁則のぶら下げをせず、必ず`avail`に収める（表のセル用）。
/// `show_urls`が真ならリンクの後ろにURLを添える。
fn wrap_inlines_opts(
    inlines: &[Inline],
    avail: usize,
    base: Style,
    theme: &Theme,
    allow_hang: bool,
    show_urls: bool,
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

    for token in tokenize(inlines, base, theme, show_urls) {
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
                let hang = allow_hang && cur_w > 0 && space_w == 0 && is_no_line_start(&text);
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
    fn headings_are_recorded_with_line_index() {
        let rendered = render_with(
            &parse("# A\n\ntext\n\n## B *c*\n"),
            20,
            &Theme::default(),
            None,
            &Options::default(),
        );
        assert_eq!(
            rendered.headings,
            vec![
                Heading {
                    level: 1,
                    text: "A".into(),
                    line: 0
                },
                // 見出しの下の罫線1行を挟んで: A, ━, 空, text, 空, B
                Heading {
                    level: 2,
                    text: "B c".into(),
                    line: 5
                },
            ]
        );
        assert_eq!(plain(&rendered.lines)[5], "B c");
    }

    #[test]
    fn wide_table_wraps_cells_within_width() {
        let md = "| 名前 | 説明 |\n|---|---|\n| folio | ターミナルで文書を読み、そのまま編集に入るための道具。 |\n";
        let lines = render(&parse(md), 30);
        assert!(
            widths(&lines).iter().all(|w| *w <= 30),
            "{:?}",
            plain(&lines)
        );
        // 説明セルが複数行に割れ、名前セルは空白で埋まる
        assert!(lines.len() > 3);
        assert!(plain(&lines)[3].starts_with("      │ "));
    }

    #[test]
    fn fit_columns_keeps_natural_when_it_fits_and_scales_otherwise() {
        assert_eq!(fit_columns(&[5, 10], 20), vec![5, 10]);
        // 短い列（10以下）は自然幅のまま、広い列が縮む
        assert_eq!(fit_columns(&[5, 52], 27), vec![5, 22]);
        // 広い列同士は比で配分し、合計をぴったり合わせる
        let w = fit_columns(&[20, 40], 30);
        assert_eq!(w.iter().sum::<usize>(), 30);
        assert!(w[1] > w[0] && w[0] >= MIN_COL_WIDTH);
        // 最小幅すら入らない時は均等（はみ出しは許容）
        assert_eq!(fit_columns(&[10, 10, 10], 6), vec![4, 4, 4]);
    }

    #[test]
    fn show_urls_appends_url_after_link_text() {
        let blocks = parse("see [the **doc**](http://x.y) now");
        let off = render(&blocks, 60);
        assert_eq!(plain(&off), vec!["see the doc now"]);
        let on = render_with(
            &blocks,
            60,
            &Theme::default(),
            None,
            &Options { show_urls: true },
        );
        assert_eq!(plain(&on.lines), vec!["see the doc (http://x.y) now"]);
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
