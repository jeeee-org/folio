//! PDFの文字を抜き出し、ページごとのブロック構造にする。
//! 抜き出しはpopplerの`pdftotext`に任せる（日本語に強い。選定理由はNOTES.md）。
//! 段組み（`-layout`）は使わず読む順に抜き出し、端末の幅で折り返し直す。

use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};
use unicode_width::UnicodeWidthStr;

use crate::document::{Block, Inline};

/// ページの区切り（`pdftotext`はページの終わりに改ページ文字を出す）。
const PAGE_BREAK: char = '\x0c';

/// 行がページの最も長い行のこの割合以上なら、右端まで埋まって折り返された行とみなす。
const FULL_LINE_RATIO: f64 = 0.8;

/// `pdftotext`でPDFの文字を抜き出す。
pub fn extract(path: &Path) -> Result<String> {
    let output = match Command::new("pdftotext")
        .args(["-enc", "UTF-8", "-q"])
        .arg(path)
        .arg("-")
        .output()
    {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "PDFを読むにはpdftotextが要ります（Debian/Ubuntuならsudo apt install poppler-utils）"
        ),
        Err(e) => bail!("pdftotextを起動できません: {e}"),
    };
    if !output.status.success() {
        // -qで黙らせているので、理由は終了コードで見分ける（popplerの決まり）
        let reason = match output.status.code() {
            Some(1) => "開けません（壊れているか、PDFではない）",
            Some(3) => "文字の抜き出しが許可されていません",
            _ => "読めません",
        };
        bail!("{}は{reason}", path.display());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `pdftotext`の出力をブロックにする。2ページ以上なら各ページの頭に
/// 「n / 全ページ」の見出しを置く（目次と`]]`/`[[`でページを渡れる）。
pub fn parse(text: &str) -> Vec<Block> {
    let mut pages: Vec<&str> = text.split(PAGE_BREAK).collect();
    // 最後のページの後ろの改ページ文字が作る空の切れ端を落とす
    if pages.len() > 1 && pages.last().is_some_and(|p| p.trim().is_empty()) {
        pages.pop();
    }
    let total = pages.len();
    let mut blocks = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        if total > 1 {
            blocks.push(Block::Heading {
                level: 2,
                inlines: vec![Inline::plain(format!("{} / {total}ページ", i + 1))],
            });
        }
        let paragraphs = parse_page(page);
        if paragraphs.is_empty() {
            blocks.push(Block::Paragraph(vec![Inline::plain(
                "（文字がありません。画像だけのページかもしれません）",
            )]));
        }
        blocks.extend(paragraphs);
    }
    blocks
}

/// 1ページを段落に分ける。空行で段落を分け、右端まで埋まった行は次の行とつなぐ
/// （PDFの行の折り返しを解き、端末の幅で折り返し直すため）。短い行は改行を保つ。
fn parse_page(page: &str) -> Vec<Block> {
    let lines: Vec<&str> = page.lines().map(str::trim_end).collect();
    let page_max = lines.iter().map(|l| l.width()).max().unwrap_or(0);
    let full = |line: &str| line.width() as f64 >= page_max as f64 * FULL_LINE_RATIO;

    let mut blocks = Vec::new();
    let mut current: Vec<Inline> = Vec::new();
    // 直前の行が右端まで埋まっていたか（次の行をつなぐか）
    let mut joining = false;
    for line in lines {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(Block::Paragraph(std::mem::take(&mut current)));
            }
            joining = false;
            continue;
        }
        if let Some(prev) = current.last() {
            if joining {
                if needs_space(&prev.text, line) {
                    current.push(Inline::plain(" "));
                }
            } else {
                current.push(Inline::plain(Inline::LINE_BREAK));
            }
        }
        current.push(Inline::plain(line));
        joining = full(line);
    }
    if !current.is_empty() {
        blocks.push(Block::Paragraph(current));
    }
    blocks
}

/// つなぐ2行の境目に空白が要るか。英数字どうしなら要り、日本語の側があれば要らない。
fn needs_space(prev: &str, next: &str) -> bool {
    let narrow = |c: Option<char>| c.is_some_and(|c| c.is_ascii() && !c.is_ascii_whitespace());
    narrow(prev.chars().last()) && narrow(next.chars().next()) && !prev.ends_with('-')
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn texts(block: &Block) -> String {
        let Block::Paragraph(inlines) = block else {
            panic!("段落ではない: {block:?}")
        };
        inlines
            .iter()
            .map(|i| {
                if i.text == Inline::LINE_BREAK {
                    "⏎"
                } else {
                    &i.text
                }
            })
            .collect()
    }

    #[test]
    fn single_page_has_no_page_heading() {
        let blocks = parse("一行だけ\n\x0c");
        assert_eq!(blocks.len(), 1);
        assert_eq!(texts(&blocks[0]), "一行だけ");
    }

    #[test]
    fn pages_get_numbered_headings() {
        let blocks = parse("一\n\x0c二\n\x0c");
        assert_eq!(
            blocks[0],
            Block::Heading {
                level: 2,
                inlines: vec![Inline::plain("1 / 2ページ")]
            }
        );
        assert_eq!(texts(&blocks[1]), "一");
        assert_eq!(
            blocks[2],
            Block::Heading {
                level: 2,
                inlines: vec![Inline::plain("2 / 2ページ")]
            }
        );
    }

    #[test]
    fn full_lines_are_joined_and_short_lines_keep_breaks() {
        // 1行目は右端まで埋まった行（日本語どうしは空白なしでつなぐ）、2行目で段落が終わる。
        // 3・4行目は短い箇条書きなので改行を保つ。
        let page = "あいうえおかきくけこ\nさし\n\n・短い\n・行\n";
        let blocks = parse(page);
        assert_eq!(texts(&blocks[0]), "あいうえおかきくけこさし");
        assert_eq!(texts(&blocks[1]), "・短い⏎・行");
    }

    #[test]
    fn english_lines_are_joined_with_a_space() {
        let blocks = parse("The quick brown fox\njumps.\n");
        assert_eq!(texts(&blocks[0]), "The quick brown fox jumps.");
    }

    #[test]
    fn hyphenated_line_end_is_joined_without_space() {
        assert!(!needs_space("well-", "known"));
        assert!(needs_space("fox", "jumps"));
        assert!(!needs_space("きつね", "jumps"));
    }

    #[test]
    fn page_without_text_says_so() {
        let blocks = parse("\n\x0c文字\n\x0c");
        assert!(texts(&blocks[1]).contains("文字がありません"));
        assert_eq!(texts(&blocks[3]), "文字");
    }

    #[test]
    fn extracts_text_from_a_real_pdf() {
        if Command::new("pdftotext").arg("-v").output().is_err() {
            return; // pdftotextの無い環境では確かめない
        }
        let dir = std::env::temp_dir().join(format!("folio-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hello.pdf");
        std::fs::write(&path, minimal_pdf("Hello folio")).unwrap();
        let text = extract(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(text.contains("Hello folio"), "{text:?}");
    }

    #[test]
    fn broken_pdf_is_reported() {
        if Command::new("pdftotext").arg("-v").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("folio-pdf-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.pdf");
        std::fs::write(&path, "not a pdf").unwrap();
        let err = extract(&path).unwrap_err().to_string();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(err.contains("開けません"), "{err}");
    }

    /// 1ページに英字1行だけのPDFを組み立てる（xrefの位置を数えて書く）。
    pub(crate) fn minimal_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 24 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .bytes(),
        );
        out
    }
}
