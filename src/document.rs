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
    Table {
        aligns: Vec<Align>,
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
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
}

impl Builder {
    fn event(&mut self, event: Event<'_>) {
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
            // HTMLや数式は整形しない。素のテキストとして見せる。
            Event::Html(text) | Event::InlineHtml(text) => self.text(&text),
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
            // 画像はP1では描画しない。代替テキストを目印付きで出す。
            Tag::Image { .. } => self.text("[画像: "),
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
            TagEnd::Image => self.text("]"),
            TagEnd::MetadataBlock(_)
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript => {}
        }
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
        match inlines.last_mut() {
            Some(last)
                if last.style == inline.style
                    && !last.is_line_break()
                    && !inline.is_line_break() =>
            {
                last.text.push_str(&inline.text);
            }
            _ => inlines.push(inline),
        }
    }

    fn close_leaf(&mut self) {
        let Some(leaf) = self.leaf.take() else { return };
        let block = match leaf {
            Leaf::Paragraph(inlines) => {
                if inlines.is_empty() {
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
}
