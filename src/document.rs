//! マークダウン文字列をブロック構造（[`Block`]の木）へ解析する。
//!
//! pulldown-cmarkのイベント列を、描画側が扱いやすい木に組み直すだけの層。
//! 見た目（色・記号・折り返し）はここでは決めない。

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// ブロック要素。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    /// `start`が`Some`なら番号付きリスト（その番号から始まる）。
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    CodeBlock {
        lang: Option<String>,
        code: String,
    },
    BlockQuote(Vec<Block>),
    Rule,
    /// 段落に画像だけが書かれていたもの。`src`は書かれたままのパス（解決は描画側）。
    /// `size`は`<img width height>`で指定された大きさ（`![]()`なら指定なし）
    Image {
        src: String,
        alt: String,
        size: ImageSize,
    },
    Table {
        aligns: Vec<Align>,
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
}

/// 画像の幅・高さの指定（`<img width="640">`・`width="50%"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageSize {
    pub width: Option<Length>,
    pub height: Option<Length>,
}

/// 長さ。画素か、表示欄の幅に対する割合（%）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Length {
    Px(u32),
    Percent(u32),
}

impl Length {
    /// `640`・`640px`・`50%`を読む。0以下や読めないものは`None`。
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (number, percent) = match text.strip_suffix('%') {
            Some(n) => (n, true),
            None => (text.strip_suffix("px").unwrap_or(text), false),
        };
        let n: f64 = number.trim().parse().ok()?;
        if !n.is_finite() || n < 0.5 {
            return None;
        }
        let n = n.round().min(f64::from(u32::MAX)) as u32;
        Some(if percent {
            Self::Percent(n)
        } else {
            Self::Px(n)
        })
    }

    /// 全体`whole`に対する長さ（画素）。
    pub fn of(self, whole: u32) -> u32 {
        match self {
            Self::Px(n) => n,
            Self::Percent(p) => {
                (u64::from(whole) * u64::from(p) / 100).min(u64::from(u32::MAX)) as u32
            }
        }
    }
}

/// リストの1項目。タスクリスト（`- [ ]`）ならチェック状態を持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// インライン要素。テキストの断片と、そこに効いている装飾。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inline {
    pub text: String,
    pub style: InlineStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    /// リンク先URL。表示するかどうかは描画側で決める。
    pub link: Option<String>,
}

impl Inline {
    /// 段落内の改行（ハードブレーク）を表す断片。
    pub const LINE_BREAK: &'static str = "\n";

    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: InlineStyle::default(),
        }
    }

    pub fn is_line_break(&self) -> bool {
        self.text == Self::LINE_BREAK && !self.style.code
    }
}

/// 葉に書かれた画像1枚。`[画像: `から`]`までのインラインの位置`[from, to)`とパス・大きさ。
#[derive(Debug)]
struct LeafImage {
    from: usize,
    to: usize,
    src: String,
    size: ImageSize,
}

/// 段落が画像（と空白・改行、画像を包むだけのHTMLのタグ）だけなら、画像ごとのブロックにする。
/// `wrappers`は包むだけのタグのインラインの位置。
fn images_only(inlines: &[Inline], images: &[LeafImage], wrappers: &[usize]) -> Option<Vec<Block>> {
    if images.is_empty() {
        return None;
    }
    let inside = |i: usize| images.iter().any(|m| (m.from..m.to).contains(&i));
    let only = inlines
        .iter()
        .enumerate()
        .all(|(i, inline)| inside(i) || wrappers.contains(&i) || inline.text.trim().is_empty());
    if !only {
        return None;
    }
    Some(
        images
            .iter()
            .map(|m| Block::Image {
                src: m.src.clone(),
                alt: inlines[m.from + 1..m.to - 1]
                    .iter()
                    .map(|i| i.text.as_str())
                    .collect(),
                size: m.size,
            })
            .collect(),
    )
}

/// 画像を包むだけで、段落に画像しか無いなら捨ててよいHTMLのタグ。
const WRAPPER_TAGS: &[&str] = &["p", "div", "center", "a", "picture", "source", "span", "br"];

/// HTMLのタグ`<...>`の名前（小文字）と属性。閉じタグは名前の頭に`/`が付く。
fn parse_tag(tag: &str) -> Option<(String, Vec<(String, String)>)> {
    let inner = tag.strip_prefix('<')?.strip_suffix('>')?;
    let inner = inner.strip_suffix('/').unwrap_or(inner);
    let name_end = inner
        .find(|c: char| c.is_whitespace())
        .unwrap_or(inner.len());
    let name = inner[..name_end].to_ascii_lowercase();
    let body = name.strip_prefix('/').unwrap_or(&name);
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let mut attrs = Vec::new();
    let mut rest = &inner[name_end..];
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() {
            break;
        }
        let key_end = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '/')
            .unwrap_or(rest.len());
        let key = rest[..key_end].to_ascii_lowercase();
        rest = rest[key_end..].trim_start();
        let mut value = String::new();
        if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let (v, next) = match after.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let body = &after[1..];
                    let end = body.find(q).unwrap_or(body.len());
                    (&body[..end], body.get(end + 1..).unwrap_or(""))
                }
                _ => {
                    let end = after
                        .find(|c: char| c.is_whitespace())
                        .unwrap_or(after.len());
                    (&after[..end], &after[end..])
                }
            };
            value = decode_entities(v);
            rest = next;
        }
        if key.is_empty() {
            break;
        }
        attrs.push((key, value));
    }
    Some((name, attrs))
}

/// HTMLの文字参照のうち、属性によく出るものだけをほどく。
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// パスの最後の部分（代替テキストが空の画像の目印に使う）。
fn image_file_name(src: &str) -> String {
    let name = src.rsplit(['/', '\\']).next().unwrap_or(src);
    if name.is_empty() {
        src.to_string()
    } else {
        name.to_string()
    }
}

/// マークダウン文字列を解析する。
pub fn parse(markdown: &str) -> Vec<Block> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut builder = Builder::default();
    for event in Parser::new_ext(markdown, options) {
        builder.event(event);
    }
    builder.finish()
}

/// 解析中の葉ブロック（インラインを溜めている途中のもの）。
#[derive(Debug)]
enum Leaf {
    Paragraph(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    Code { lang: Option<String>, code: String },
    TableCell(Vec<Inline>),
}

/// 解析中の入れ子コンテナ。
#[derive(Debug)]
enum Container {
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    Item {
        task: Option<bool>,
        blocks: Vec<Block>,
    },
    Table {
        aligns: Vec<Align>,
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
        in_head: bool,
        row: Vec<Vec<Inline>>,
    },
}

#[derive(Debug, Default)]
struct Builder {
    root: Vec<Block>,
    stack: Vec<Container>,
    leaf: Option<Leaf>,
    style: InlineStyle,
    /// 開いている画像（パスと、`[画像: `を置いた葉の中の位置）
    image: Option<(String, usize)>,
    /// 今の葉に書かれた画像
    leaf_images: Vec<LeafImage>,
    /// 今の葉にある、画像を包むだけのHTMLのタグのインラインの位置
    leaf_wrappers: Vec<usize>,
    /// 閉じていないHTMLのタグ（`<img`の途中で行が変わった時など）。次のHTMLと繋いで読む
    html_pending: String,
    /// 葉の中でこれより前のインラインには繋げない（画像の目印の位置がずれないように）
    seal: usize,
}

/// 文の途中の画像や読めない画像を、文字で出す時の目印。
pub const IMAGE_OPEN: &str = "[画像: ";

impl Builder {
    fn event(&mut self, event: Event<'_>) {
        if !matches!(event, Event::Html(_) | Event::InlineHtml(_)) {
            self.flush_html();
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(text) => {
                let mut style = self.style.clone();
                style.code = true;
                self.push_inline(Inline {
                    text: text.into_string(),
                    style,
                });
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.push_inline(Inline::plain(Inline::LINE_BREAK)),
            Event::Rule => {
                self.close_leaf();
                self.push_block(Block::Rule);
            }
            Event::TaskListMarker(checked) => {
                if let Some(Container::Item { task, .. }) = self.stack.last_mut() {
                    *task = Some(checked);
                }
            }
            // HTMLは`<img>`だけ画像として読み、ほかは素のテキストとして見せる。数式も整形しない
            Event::Html(text) | Event::InlineHtml(text) => self.html(&text),
            Event::InlineMath(text) | Event::DisplayMath(text) => self.text(&text),
            Event::FootnoteReference(name) => self.text(&format!("[^{name}]")),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.close_leaf();
                self.leaf = Some(Leaf::Paragraph(Vec::new()));
            }
            Tag::Heading { level, .. } => {
                self.close_leaf();
                self.leaf = Some(Leaf::Heading(heading_level(level), Vec::new()));
            }
            Tag::CodeBlock(kind) => {
                self.close_leaf();
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) if !lang.is_empty() => {
                        // ```rust,ignore のような付加情報は言語名だけ残す
                        Some(lang.split([',', ' ']).next().unwrap_or("").to_string())
                    }
                    _ => None,
                };
                self.leaf = Some(Leaf::Code {
                    lang,
                    code: String::new(),
                });
            }
            Tag::BlockQuote(_) => {
                self.close_leaf();
                self.stack.push(Container::Quote(Vec::new()));
            }
            Tag::List(start) => {
                self.close_leaf();
                self.stack.push(Container::List {
                    start,
                    items: Vec::new(),
                });
            }
            Tag::Item => {
                self.close_leaf();
                self.stack.push(Container::Item {
                    task: None,
                    blocks: Vec::new(),
                });
            }
            Tag::Table(aligns) => {
                self.close_leaf();
                self.stack.push(Container::Table {
                    aligns: aligns.into_iter().map(align).collect(),
                    header: Vec::new(),
                    rows: Vec::new(),
                    in_head: false,
                    row: Vec::new(),
                });
            }
            Tag::TableHead => {
                if let Some(Container::Table { in_head, .. }) = self.stack.last_mut() {
                    *in_head = true;
                }
            }
            Tag::TableRow => {}
            Tag::TableCell => self.leaf = Some(Leaf::TableCell(Vec::new())),
            Tag::Emphasis => self.style.italic = true,
            Tag::Strong => self.style.bold = true,
            Tag::Strikethrough => self.style.strike = true,
            Tag::Link { dest_url, .. } => self.style.link = Some(dest_url.into_string()),
            // 画像は代替テキストを目印付きで出しておき、段落に画像しか無ければ閉じる時に
            // Block::Imageへ置き換える（文の途中の画像は文字のまま）
            Tag::Image { dest_url, .. } => {
                self.seal = self.leaf_len();
                self.push_inline(Inline::plain(IMAGE_OPEN));
                let at = self.leaf_len().saturating_sub(1);
                self.seal = self.leaf_len();
                self.image = Some((dest_url.into_string(), at));
            }
            Tag::HtmlBlock => {
                self.close_leaf();
                self.leaf = Some(Leaf::Paragraph(Vec::new()));
            }
            Tag::FootnoteDefinition(name) => {
                self.close_leaf();
                self.leaf = Some(Leaf::Paragraph(vec![Inline::plain(format!("[^{name}]: "))]));
            }
            Tag::MetadataBlock(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::CodeBlock
            | TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition => self.close_leaf(),
            TagEnd::BlockQuote(_) => {
                self.close_leaf();
                if let Some(Container::Quote(blocks)) = self.stack.pop() {
                    self.push_block(Block::BlockQuote(blocks));
                }
            }
            TagEnd::List(_) => {
                self.close_leaf();
                if let Some(Container::List { start, items }) = self.stack.pop() {
                    self.push_block(Block::List { start, items });
                }
            }
            TagEnd::Item => {
                self.close_leaf();
                if let Some(Container::Item { task, blocks }) = self.stack.pop()
                    && let Some(Container::List { items, .. }) = self.stack.last_mut()
                {
                    items.push(ListItem { task, blocks });
                }
            }
            TagEnd::Table => {
                self.close_leaf();
                if let Some(Container::Table {
                    aligns,
                    header,
                    rows,
                    ..
                }) = self.stack.pop()
                {
                    self.push_block(Block::Table {
                        aligns,
                        header,
                        rows,
                    });
                }
            }
            TagEnd::TableHead => {
                if let Some(Container::Table {
                    in_head,
                    header,
                    row,
                    ..
                }) = self.stack.last_mut()
                {
                    *in_head = false;
                    *header = std::mem::take(row);
                }
            }
            TagEnd::TableRow => {
                if let Some(Container::Table { rows, row, .. }) = self.stack.last_mut() {
                    rows.push(std::mem::take(row));
                }
            }
            TagEnd::TableCell => {
                // セルの中の画像は文字のまま
                self.flush_html();
                self.leaf_images.clear();
                self.leaf_wrappers.clear();
                self.image = None;
                self.seal = 0;
                if let Some(Leaf::TableCell(inlines)) = self.leaf.take()
                    && let Some(Container::Table { row, .. }) = self.stack.last_mut()
                {
                    row.push(inlines);
                }
            }
            TagEnd::Emphasis => self.style.italic = false,
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Strikethrough => self.style.strike = false,
            TagEnd::Link => self.style.link = None,
            TagEnd::Image => self.end_image(),
            TagEnd::MetadataBlock(_)
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript => {}
        }
    }

    /// 今の葉のインラインの数。
    fn leaf_len(&self) -> usize {
        match &self.leaf {
            Some(Leaf::Paragraph(v) | Leaf::Heading(_, v) | Leaf::TableCell(v)) => v.len(),
            _ => 0,
        }
    }

    fn end_image(&mut self) {
        let Some((src, from)) = self.image.take() else {
            self.text("]");
            return;
        };
        if self.leaf_len() == from + 1 {
            // 代替テキストが空ならファイル名を出す
            self.push_inline(Inline::plain(image_file_name(&src)));
        }
        self.seal = self.leaf_len();
        self.push_inline(Inline::plain("]"));
        self.seal = self.leaf_len();
        self.leaf_images.push(LeafImage {
            from,
            to: self.leaf_len(),
            src,
            size: ImageSize::default(),
        });
    }

    /// HTMLの断片。`<img>`は画像に、画像を包むだけのタグは目印を付けて素のまま、
    /// ほかは素のテキストにする。閉じていないタグは次の断片と繋いで読む。
    fn html(&mut self, text: &str) {
        if let Some(Leaf::Code { code, .. }) = &mut self.leaf {
            code.push_str(text);
            return;
        }
        let mut buf = std::mem::take(&mut self.html_pending);
        buf.push_str(text);
        let mut rest = buf.as_str();
        while !rest.is_empty() {
            let Some(lt) = rest.find('<') else {
                self.text(rest);
                break;
            };
            if lt > 0 {
                self.text(&rest[..lt]);
            }
            rest = &rest[lt..];
            let Some(gt) = rest.find('>') else {
                self.html_pending = rest.to_string();
                break;
            };
            self.html_tag(&rest[..=gt]);
            rest = &rest[gt + 1..];
        }
    }

    /// 閉じないまま残ったHTMLを素のテキストで出す。
    fn flush_html(&mut self) {
        let rest = std::mem::take(&mut self.html_pending);
        if !rest.is_empty() {
            self.text(&rest);
        }
    }

    fn html_tag(&mut self, tag: &str) {
        let Some((name, attrs)) = parse_tag(tag) else {
            self.text(tag);
            return;
        };
        let attr = |key: &str| {
            attrs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        if name == "img"
            && let Some(src) = attr("src").filter(|s| !s.trim().is_empty())
        {
            let size = ImageSize {
                width: attr("width").and_then(Length::parse),
                height: attr("height").and_then(Length::parse),
            };
            let src = src.trim().to_string();
            let alt = attr("alt").unwrap_or("").trim();
            let alt = if alt.is_empty() {
                image_file_name(&src)
            } else {
                alt.to_string()
            };
            self.seal = self.leaf_len();
            self.push_inline(Inline::plain(IMAGE_OPEN));
            let from = self.leaf_len().saturating_sub(1);
            self.seal = self.leaf_len();
            self.push_inline(Inline::plain(alt));
            self.seal = self.leaf_len();
            self.push_inline(Inline::plain("]"));
            self.seal = self.leaf_len();
            self.leaf_images.push(LeafImage {
                from,
                to: self.leaf_len(),
                src,
                size,
            });
            return;
        }
        let body = name.strip_prefix('/').unwrap_or(&name);
        if WRAPPER_TAGS.contains(&body) {
            self.seal = self.leaf_len();
            self.text(tag);
            let at = self.leaf_len().saturating_sub(1);
            self.seal = self.leaf_len();
            self.leaf_wrappers.push(at);
            return;
        }
        self.text(tag);
    }

    fn text(&mut self, text: &str) {
        if let Some(Leaf::Code { code, .. }) = &mut self.leaf {
            code.push_str(text);
            return;
        }
        self.push_inline(Inline {
            text: text.to_string(),
            style: self.style.clone(),
        });
    }

    /// インラインを現在の葉に足す。葉が無ければ（タイトなリスト項目など）
    /// 暗黙の段落を開く。
    fn push_inline(&mut self, inline: Inline) {
        let inlines = match &mut self.leaf {
            Some(Leaf::Paragraph(v) | Leaf::Heading(_, v) | Leaf::TableCell(v)) => v,
            Some(Leaf::Code { code, .. }) => {
                code.push_str(&inline.text);
                return;
            }
            None => {
                self.leaf = Some(Leaf::Paragraph(Vec::new()));
                match &mut self.leaf {
                    Some(Leaf::Paragraph(v)) => v,
                    _ => unreachable!(),
                }
            }
        };
        // 同じ装飾の断片は繋げておく（描画側の折り返しが楽になる）
        let sealed = inlines.len() <= self.seal;
        match inlines.last_mut() {
            Some(last)
                if !sealed
                    && last.style == inline.style
                    && !last.is_line_break()
                    && !inline.is_line_break() =>
            {
                last.text.push_str(&inline.text);
            }
            _ => inlines.push(inline),
        }
    }

    fn close_leaf(&mut self) {
        self.flush_html();
        let images = std::mem::take(&mut self.leaf_images);
        let wrappers = std::mem::take(&mut self.leaf_wrappers);
        self.image = None;
        self.seal = 0;
        let Some(leaf) = self.leaf.take() else { return };
        let block = match leaf {
            Leaf::Paragraph(inlines) => {
                if inlines.is_empty() {
                    return;
                }
                if let Some(blocks) = images_only(&inlines, &images, &wrappers) {
                    for block in blocks {
                        self.push_block(block);
                    }
                    return;
                }
                Block::Paragraph(inlines)
            }
            Leaf::Heading(level, inlines) => Block::Heading { level, inlines },
            Leaf::Code { lang, mut code } => {
                // 末尾の改行は行数に数えない
                if code.ends_with('\n') {
                    code.pop();
                }
                Block::CodeBlock { lang, code }
            }
            // セルはTableCellの終了イベントで回収する。ここに来るのは異常系
            Leaf::TableCell(_) => return,
        };
        self.push_block(block);
    }

    fn push_block(&mut self, block: Block) {
        let target = match self.stack.last_mut() {
            Some(Container::Quote(blocks) | Container::Item { blocks, .. }) => blocks,
            // ListやTableの直下にブロックは来ない。来たら親へ逃がす
            Some(Container::List { .. } | Container::Table { .. }) | None => &mut self.root,
        };
        target.push(block);
    }

    fn finish(mut self) -> Vec<Block> {
        self.close_leaf();
        while let Some(container) = self.stack.pop() {
            match container {
                Container::Quote(blocks) => self.push_block(Block::BlockQuote(blocks)),
                Container::List { start, items } => self.push_block(Block::List { start, items }),
                Container::Item { task, blocks } => {
                    if let Some(Container::List { items, .. }) = self.stack.last_mut() {
                        items.push(ListItem { task, blocks });
                    }
                }
                Container::Table {
                    aligns,
                    header,
                    rows,
                    ..
                } => self.push_block(Block::Table {
                    aligns,
                    header,
                    rows,
                }),
            }
        }
        self.root
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn align(a: Alignment) -> Align {
    match a {
        Alignment::Center => Align::Center,
        Alignment::Right => Align::Right,
        Alignment::None | Alignment::Left => Align::Left,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(inlines: &[Inline]) -> String {
        inlines.iter().map(|i| i.text.as_str()).collect()
    }

    #[test]
    fn heading_and_paragraph() {
        let blocks = parse("# 見出し\n\n本文です。\n続き。\n");
        assert_eq!(blocks.len(), 2);
        assert!(
            matches!(&blocks[0], Block::Heading { level: 1, inlines } if text_of(inlines) == "見出し")
        );
        // ソフトブレークは空白になる
        assert!(
            matches!(&blocks[1], Block::Paragraph(inlines) if text_of(inlines) == "本文です。 続き。")
        );
    }

    #[test]
    fn inline_styles_are_split() {
        let blocks = parse("a **b** `c` [d](http://x)");
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!()
        };
        assert_eq!(inlines.len(), 6);
        assert!(inlines[1].style.bold && inlines[1].text == "b");
        assert!(inlines[3].style.code && inlines[3].text == "c");
        assert_eq!(inlines[5].style.link.as_deref(), Some("http://x"));
    }

    #[test]
    fn tight_list_items_get_implicit_paragraph() {
        let blocks = parse("- one\n- [x] two\n  - nested\n");
        let Block::List { start: None, items } = &blocks[0] else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0].blocks[0], Block::Paragraph(v) if text_of(v) == "one"));
        assert_eq!(items[1].task, Some(true));
        assert!(matches!(&items[1].blocks[1], Block::List { .. }));
    }

    #[test]
    fn fenced_code_keeps_lang_and_drops_trailing_newline() {
        let blocks = parse("```rust,ignore\nfn main() {}\n```\n");
        assert_eq!(
            blocks[0],
            Block::CodeBlock {
                lang: Some("rust".into()),
                code: "fn main() {}".into()
            }
        );
    }

    #[test]
    fn table_has_header_and_rows() {
        let blocks = parse("| a | b |\n|---|--:|\n| 1 | 2 |\n| 3 | 4 |\n");
        let Block::Table {
            aligns,
            header,
            rows,
        } = &blocks[0]
        else {
            panic!()
        };
        assert_eq!(aligns, &[Align::Left, Align::Right]);
        assert_eq!(header.len(), 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(text_of(&rows[1][1]), "4");
    }

    #[test]
    fn quote_and_rule() {
        let blocks = parse("> 引用\n\n---\n");
        assert!(matches!(&blocks[0], Block::BlockQuote(inner) if inner.len() == 1));
        assert_eq!(blocks[1], Block::Rule);
    }

    #[test]
    fn image_alone_in_paragraph_becomes_image_block() {
        let blocks = parse("前\n\n![](../../a/5-点と費用.png)\n\n後\n");
        assert_eq!(
            blocks[1],
            Block::Image {
                src: "../../a/5-点と費用.png".into(),
                alt: "5-点と費用.png".into(),
                size: ImageSize::default(),
            }
        );
        // 2枚続けて書けば2つのブロック。リンクで囲っても画像として扱う
        let blocks = parse("![図 *1*](a.png)\n[![](b.png)](http://x)\n");
        assert_eq!(
            blocks,
            vec![
                Block::Image {
                    src: "a.png".into(),
                    alt: "図 1".into(),
                    size: ImageSize::default(),
                },
                Block::Image {
                    src: "b.png".into(),
                    alt: "b.png".into(),
                    size: ImageSize::default(),
                },
            ]
        );
    }

    #[test]
    fn image_inside_text_stays_as_text() {
        let blocks = parse("前の文 ![図](a.png) 後の文\n");
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(text_of(inlines), "前の文 [画像: 図] 後の文");
        // 表のセルの中も文字のまま
        let blocks = parse("|a|\n|-|\n|![](x.png)|\n");
        let Block::Table { rows, .. } = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(text_of(&rows[0][0]), "[画像: x.png]");
    }

    #[test]
    fn length_is_pixels_or_percent() {
        assert_eq!(Length::parse("640"), Some(Length::Px(640)));
        assert_eq!(Length::parse(" 640px "), Some(Length::Px(640)));
        assert_eq!(Length::parse("50%"), Some(Length::Percent(50)));
        assert_eq!(Length::parse("33.4%"), Some(Length::Percent(33)));
        assert_eq!(Length::parse("0"), None);
        assert_eq!(Length::parse("auto"), None);
        assert_eq!(Length::parse("10em"), None);
        assert_eq!(Length::Percent(50).of(800), 400);
        assert_eq!(Length::Px(640).of(800), 640);
    }

    #[test]
    fn img_tag_becomes_image_block_with_size() {
        let blocks = parse(
            "前\n\n<img src=\"図/a%20b.png\" width=\"640\" alt=\"画面 1\">\n\n後 <img src='c.png' WIDTH=50% height=200px/>\n",
        );
        assert_eq!(
            blocks[1],
            Block::Image {
                src: "図/a%20b.png".into(),
                alt: "画面 1".into(),
                size: ImageSize {
                    width: Some(Length::Px(640)),
                    height: None,
                },
            }
        );
        // 文の途中のimgは文字のまま（代替テキストが無ければファイル名）
        let Block::Paragraph(inlines) = &blocks[2] else {
            panic!("{blocks:?}");
        };
        assert_eq!(text_of(inlines), "後 [画像: c.png]");
        // 段落にimgだけなら、行の途中でも画像。大きさの指定が無ければ指定なし
        let blocks = parse("text\n\n<img src=\"a.png\"> <img src=\"b.png\" height=\"120\">\n");
        assert_eq!(
            blocks[1..],
            [
                Block::Image {
                    src: "a.png".into(),
                    alt: "a.png".into(),
                    size: ImageSize::default(),
                },
                Block::Image {
                    src: "b.png".into(),
                    alt: "b.png".into(),
                    size: ImageSize {
                        width: None,
                        height: Some(Length::Px(120)),
                    },
                },
            ]
        );
    }

    #[test]
    fn img_tag_wrapped_in_html_or_split_over_lines() {
        // 中央寄せのpやリンクで包んでも画像。タグが行をまたいでもよい
        let blocks = parse(
            "<p align=\"center\">\n  <a href=\"x\"><img\n    src=\"a.png\"\n    width=\"30%\"></a>\n</p>\n",
        );
        assert_eq!(
            blocks,
            vec![Block::Image {
                src: "a.png".into(),
                alt: "a.png".into(),
                size: ImageSize {
                    width: Some(Length::Percent(30)),
                    height: None,
                },
            }]
        );
        // 画像の無いHTMLは今までどおり素のまま見せる
        let blocks = parse("<div>\nこんにちは\n</div>\n");
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(text_of(inlines), "<div>\nこんにちは\n</div>\n");
        // srcの無いimgは消さずに素のまま
        let blocks = parse("a <img alt=\"x\"> b\n");
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(text_of(inlines), "a <img alt=\"x\"> b");
    }
}
