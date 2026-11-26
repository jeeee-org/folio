//! ファイルの形式（マークダウン・ソースコード・CSV/TSV・プレーンテキスト・PDF・画像）の判定と、
//! 各形式をブロック構造へ変換する入口。中核の一部で、ターミナルには触らない。

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};

use crate::document::{self, Block, Inline};
use crate::pdf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Code,
    Csv,
    Tsv,
    Text,
    /// 文字だけを抜き出して読む（中身は`pdftotext`の出力）
    Pdf,
    /// 画像ファイル（見た目でだけ出す）
    Image,
}

impl Format {
    /// 拡張子で決める。`knows_ext`はハイライターがその拡張子を言語として知っているか。
    pub fn detect(path: &Path, knows_ext: impl Fn(&str) -> bool) -> Format {
        let ext = extension(path);
        match ext.as_str() {
            "md" | "markdown" | "mdown" | "mkd" => Format::Markdown,
            "csv" => Format::Csv,
            "tsv" => Format::Tsv,
            "pdf" => Format::Pdf,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "svgz" => Format::Image,
            "txt" | "text" | "" => Format::Text,
            e if knows_ext(e) => Format::Code,
            _ => Format::Text,
        }
    }

    /// 形式に応じてブロック構造にする。`path`はソースコードの言語判定に使う。
    pub fn parse(self, path: &Path, source: &str) -> Vec<Block> {
        match self {
            Format::Markdown => document::parse(source),
            Format::Code => vec![Block::CodeBlock {
                lang: Some(extension(path)).filter(|e| !e.is_empty()),
                code: source.to_string(),
            }],
            Format::Csv => parse_delimited(source, b','),
            Format::Tsv => parse_delimited(source, b'\t'),
            Format::Text => parse_text(source),
            Format::Pdf => pdf::parse(source),
            Format::Image => vec![Block::Paragraph(vec![Inline::plain(
                "（画像ファイル。folio viewかファイラーで見た目を出せます）",
            )])],
        }
    }
}

impl FromStr for Format {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "markdown" | "md" => Ok(Format::Markdown),
            "code" => Ok(Format::Code),
            "csv" => Ok(Format::Csv),
            "tsv" => Ok(Format::Tsv),
            "text" | "txt" => Ok(Format::Text),
            "pdf" => Ok(Format::Pdf),
            "image" => Ok(Format::Image),
            other => Err(format!(
                "{other}は形式として解釈できません（markdown / code / csv / tsv / text / pdf / image）"
            )),
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Format::Markdown => "markdown",
            Format::Code => "code",
            Format::Csv => "csv",
            Format::Tsv => "tsv",
            Format::Text => "text",
            Format::Pdf => "pdf",
            Format::Image => "image",
        };
        f.write_str(name)
    }
}

/// 形式に応じてファイルの中身を文字列で読む。PDFは文字を抜き出し、他はUTF-8として読む
/// （文字コードの自動判定はしない）。`format`が無ければ拡張子で決める。
pub fn read_source(path: &Path, format: Option<Format>) -> Result<String> {
    match resolve(path, format) {
        Format::Pdf => return pdf::extract(path),
        Format::Image => return Ok(String::new()), // 文字としては読まない
        _ => {}
    }
    let bytes = std::fs::read(path).with_context(|| format!("{}を読めません", path.display()))?;
    String::from_utf8(bytes).with_context(|| {
        format!(
            "{}はUTF-8ではありません（他の文字コードには未対応）",
            path.display()
        )
    })
}

/// PDFとして読むか。形式の指定が無ければ拡張子で決める。
pub fn is_pdf(path: &Path, format: Option<Format>) -> bool {
    resolve(path, format) == Format::Pdf
}

/// 指定が無ければ拡張子で決めた形式。ソースコードかテキストかは区別しない
/// （PDF・画像かどうかを知りたい時に、ハイライターを用意せずに使う）。
pub fn resolve(path: &Path, format: Option<Format>) -> Format {
    format.unwrap_or_else(|| Format::detect(path, |_| false))
}

impl Format {
    /// 見た目（画像）で出せる形式か。
    pub fn is_visual(self) -> bool {
        matches!(self, Format::Pdf | Format::Image)
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// プレーンテキスト。空行で段落を分け、段落内の改行は保つ（幅で折り返される）。
fn parse_text(source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current: Vec<Inline> = Vec::new();
    for line in source.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(Block::Paragraph(std::mem::take(&mut current)));
            }
            continue;
        }
        if !current.is_empty() {
            current.push(Inline::plain(Inline::LINE_BREAK));
        }
        current.push(Inline::plain(line));
    }
    if !current.is_empty() {
        blocks.push(Block::Paragraph(current));
    }
    blocks
}

/// CSV/TSV。1行目を見出しとする表。引用符・セル内の区切り文字と改行に対応。
fn parse_delimited(source: &str, delimiter: u8) -> Vec<Block> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(source.as_bytes());
    let mut records: Vec<Vec<String>> = Vec::new();
    for record in reader.records() {
        match record {
            Ok(r) => records.push(r.iter().map(str::to_string).collect()),
            Err(err) => {
                return vec![Block::Paragraph(vec![Inline::plain(format!(
                    "読み取りに失敗しました: {err}"
                ))])];
            }
        }
    }
    let Some(header) = records.first() else {
        return vec![Block::Paragraph(vec![Inline::plain("（空のファイル）")])];
    };
    let ncols = records.iter().map(Vec::len).max().unwrap_or(0);
    let cell = |s: &str| -> Vec<Inline> {
        // セル内の改行は表の行を崩すので空白にする
        vec![Inline::plain(s.replace(['\r', '\n'], " "))]
    };
    let row = |r: &Vec<String>| -> Vec<Vec<Inline>> {
        (0..ncols)
            .map(|c| cell(r.get(c).map(String::as_str).unwrap_or("")))
            .collect()
    };
    vec![Block::Table {
        aligns: vec![document::Align::Left; ncols],
        header: row(header),
        rows: records[1..].iter().map(row).collect(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn knows(ext: &str) -> bool {
        matches!(ext, "rs" | "py" | "json")
    }

    #[test]
    fn detects_by_extension_and_highlighter_knowledge() {
        let d = |p: &str| Format::detect(Path::new(p), knows);
        assert_eq!(d("a.md"), Format::Markdown);
        assert_eq!(d("a.MD"), Format::Markdown);
        assert_eq!(d("a.csv"), Format::Csv);
        assert_eq!(d("a.tsv"), Format::Tsv);
        assert_eq!(d("a.rs"), Format::Code);
        assert_eq!(d("a.json"), Format::Code);
        assert_eq!(d("a.unknownext"), Format::Text);
        assert_eq!(d("LICENSE"), Format::Text);
        assert_eq!(d("a.txt"), Format::Text);
        assert_eq!(d("a.PDF"), Format::Pdf);
        assert_eq!(d("a.JPG"), Format::Image);
        assert_eq!(d("a.webp"), Format::Image);
        assert_eq!(d("a.SVG"), Format::Image);
        assert_eq!(d("a.svgz"), Format::Image);
    }

    #[test]
    fn format_from_str_accepts_aliases() {
        assert_eq!("md".parse::<Format>().unwrap(), Format::Markdown);
        assert_eq!("TSV".parse::<Format>().unwrap(), Format::Tsv);
        assert_eq!("pdf".parse::<Format>().unwrap(), Format::Pdf);
        assert!("xlsx".parse::<Format>().is_err());
    }

    #[test]
    fn text_keeps_line_breaks_and_splits_paragraphs_on_blank_lines() {
        let blocks = Format::Text.parse(Path::new("x"), "a\nb\n\n\nc\n");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph(vec![
                    Inline::plain("a"),
                    Inline::plain(Inline::LINE_BREAK),
                    Inline::plain("b")
                ]),
                Block::Paragraph(vec![Inline::plain("c")]),
            ]
        );
    }

    #[test]
    fn code_is_one_block_with_extension_as_lang() {
        let blocks = Format::Code.parse(Path::new("main.rs"), "fn main() {}\n");
        assert_eq!(
            blocks,
            vec![Block::CodeBlock {
                lang: Some("rs".into()),
                code: "fn main() {}\n".into()
            }]
        );
    }

    #[test]
    fn csv_with_quotes_and_ragged_rows_becomes_table() {
        let src = "name,note\n\"Smith, J\",\"says \"\"hi\"\"\nline2\"\nonly\n";
        let blocks = Format::Csv.parse(Path::new("x.csv"), src);
        let Block::Table { header, rows, .. } = &blocks[0] else {
            panic!()
        };
        assert_eq!(header[0][0].text, "name");
        assert_eq!(rows[0][0][0].text, "Smith, J");
        assert_eq!(rows[0][1][0].text, "says \"hi\" line2");
        // 列が足りない行は空セルで埋める
        assert_eq!(rows[1][1][0].text, "");
    }
}
