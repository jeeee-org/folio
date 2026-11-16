//! 画像を端末に描く部品。画像ファイルとPDFのページを、端末の描き方（Kitty方式・sixel、
//! 描けなければブロック文字）に合わせて用意する。端末の問い合わせは起動時に1回だけ。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};
use image::DynamicImage;
use image::ImageDecoder;
use image::imageops::FilterType;
use ratatui::layout::{Rect, Size};
use ratatui_image::Resize;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;

use crate::format::Format;

static PICKER: OnceLock<Picker> = OnceLock::new();

/// 覚えておくページの数（行き来した時に描き直さない）。
const CACHE_PAGES: usize = 8;

/// 端末に描き方を問い合わせる。TUIを始めた直後（キーを読む前）に1回だけ呼ぶ。
/// 環境変数`FOLIO_GRAPHICS`（`kitty` / `sixel` / `iterm2` / `halfblocks`）で描き方を決め打ちできる。
pub fn init() {
    if PICKER.get().is_some() {
        return;
    }
    let forced = std::env::var("FOLIO_GRAPHICS").ok();
    let mut picker = if forced.as_deref() == Some("halfblocks") {
        Picker::halfblocks()
    } else {
        Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
    };
    match forced.as_deref() {
        Some("kitty") => picker.set_protocol_type(ProtocolType::Kitty),
        Some("sixel") => picker.set_protocol_type(ProtocolType::Sixel),
        Some("iterm2") => picker.set_protocol_type(ProtocolType::Iterm2),
        _ => {}
    }
    let _ = PICKER.set(picker);
}

fn picker() -> &'static Picker {
    PICKER.get_or_init(Picker::halfblocks)
}

/// 画素で描ける端末か（ブロック文字での近似しかできないなら`false`）。
pub fn can_draw() -> bool {
    picker().protocol_type() != ProtocolType::Halfblocks
}

/// 描き方の名前（ステータス行に出す）。
pub fn protocol_name() -> &'static str {
    match picker().protocol_type() {
        ProtocolType::Kitty => "Kitty方式",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Iterm2 => "iTerm2方式",
        ProtocolType::Halfblocks => "ブロック文字",
    }
}

/// 見た目で出す文書（画像ファイル1枚か、PDFのページの並び）。
pub struct Visual {
    path: PathBuf,
    kind: Kind,
    /// 描いたページ（ページ番号・欄の大きさ・描いたもの）。新しいものほど後ろ
    cache: Vec<(usize, Size, Protocol)>,
}

enum Kind {
    /// 読み込んだ画像（向きは直してある）
    Image(DynamicImage),
    /// ページごとの大きさ（ポイント。回転を反映した幅・高さ）
    Pdf(Vec<(f64, f64)>),
}

impl Visual {
    /// 画像ファイルかPDFを開く。PDFはページ数と大きさだけを先に読む。
    pub fn open(path: &Path, format: Format) -> Result<Self> {
        let kind = match format {
            Format::Pdf => Kind::Pdf(pdf_page_sizes(path)?),
            _ => Kind::Image(load_image(path)?),
        };
        Ok(Self {
            path: path.to_path_buf(),
            kind,
            cache: Vec::new(),
        })
    }

    pub fn is_pdf(&self) -> bool {
        matches!(self.kind, Kind::Pdf(_))
    }

    pub fn page_count(&self) -> usize {
        match &self.kind {
            Kind::Image(_) => 1,
            Kind::Pdf(pages) => pages.len(),
        }
    }

    /// 画像の画素数（画像ファイルだけ）。
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        match &self.kind {
            Kind::Image(img) => Some((img.width(), img.height())),
            Kind::Pdf(_) => None,
        }
    }

    /// そのページをその大きさで描いたものを覚えているか。
    pub fn is_cached(&self, page: usize, area: Size) -> bool {
        self.cache.iter().any(|(p, s, _)| *p == page && *s == area)
    }

    /// `page`（0始まり）を`area`の大きさ（桁・行）に収めて描けるようにする。描いたものは覚えておく。
    pub fn page(&mut self, page: usize, area: Size) -> Result<&Protocol> {
        if let Some(i) = self
            .cache
            .iter()
            .position(|(p, s, _)| *p == page && *s == area)
        {
            let hit = self.cache.remove(i);
            self.cache.push(hit);
        } else {
            let protocol = self.make(page, area)?;
            if self.cache.len() >= CACHE_PAGES {
                self.cache.remove(0);
            }
            self.cache.push((page, area, protocol));
        }
        Ok(&self.cache.last().expect("直前に入れた").2)
    }

    fn make(&self, page: usize, area: Size) -> Result<Protocol> {
        let picker = picker();
        let font = picker.font_size();
        let (max_w, max_h) = (
            u32::from(area.width) * u32::from(font.width),
            u32::from(area.height) * u32::from(font.height),
        );
        if max_w == 0 || max_h == 0 {
            bail!("描く場所がありません");
        }
        let image = match &self.kind {
            Kind::Image(img) => img.clone(),
            Kind::Pdf(pages) => {
                let &(w, h) = pages
                    .get(page)
                    .ok_or_else(|| anyhow!("{}ページ目はありません", page + 1))?;
                let (pw, ph) = fit(w, h, max_w, max_h);
                render_pdf_page(&self.path, page, pw, ph)?
            }
        };
        picker
            .new_protocol(image, area, Resize::Fit(Some(FilterType::Triangle)))
            .map_err(|e| anyhow!("画像にできません: {e}"))
    }
}

/// 描いたものを`area`の中央に置く場所。
pub fn centered(protocol: &Protocol, area: Rect) -> Rect {
    let size = protocol.size();
    let width = size.width.min(area.width);
    let height = size.height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// 幅`w`・高さ`h`のものを、縦横比を保って`max_w`×`max_h`の画素に収める大きさ。
fn fit(w: f64, h: f64, max_w: u32, max_h: u32) -> (u32, u32) {
    if w <= 0.0 || h <= 0.0 {
        return (max_w, max_h);
    }
    let scale = (f64::from(max_w) / w).min(f64::from(max_h) / h);
    (
        ((w * scale).floor() as u32).max(1),
        ((h * scale).floor() as u32).max(1),
    )
}

/// 画像ファイルを読み、写真の向き（EXIF）を直す。
fn load_image(path: &Path) -> Result<DynamicImage> {
    let reader = image::ImageReader::open(path)
        .with_context(|| format!("{}を読めません", path.display()))?
        .with_guessed_format()?;
    let mut decoder = reader
        .into_decoder()
        .with_context(|| format!("{}は画像として読めません", path.display()))?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder)
        .with_context(|| format!("{}は画像として読めません", path.display()))?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

/// `pdfinfo`でページごとの大きさを読む（回転していれば幅と高さを入れ替える）。
fn pdf_page_sizes(path: &Path) -> Result<Vec<(f64, f64)>> {
    let output = run_poppler(
        Command::new("pdfinfo")
            .args(["-f", "1", "-l", "100000"])
            .arg(path),
        "pdfinfo",
    )?;
    let text = String::from_utf8_lossy(&output);
    let pages = parse_page_sizes(&text);
    if pages.is_empty() {
        bail!("{}のページを読めません", path.display());
    }
    Ok(pages)
}

/// `pdfinfo -f 1 -l N`の出力から、ページごとの（幅, 高さ）を取り出す。
fn parse_page_sizes(text: &str) -> Vec<(f64, f64)> {
    let mut sizes: Vec<(f64, f64)> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("Page") else {
            continue;
        };
        let Some((_, value)) = rest.split_once(':') else {
            continue;
        };
        if rest.contains(" size:") {
            // "  594.96 x 841.92 pts (A4)"
            let mut nums = value.split_whitespace();
            let w = nums.next().and_then(|s| s.parse().ok());
            let _x = nums.next();
            let h = nums.next().and_then(|s| s.parse().ok());
            if let (Some(w), Some(h)) = (w, h) {
                sizes.push((w, h));
            }
        } else if rest.contains(" rot:")
            && let Ok(rot) = value.trim().parse::<i32>()
            && rot.rem_euclid(180) == 90
            && let Some(last) = sizes.last_mut()
        {
            *last = (last.1, last.0);
        }
    }
    sizes
}

/// `pdftoppm`で1ページを`w`×`h`の画素に描く（PPMで受け取る。PNGより速い）。
fn render_pdf_page(path: &Path, page: usize, w: u32, h: u32) -> Result<DynamicImage> {
    let n = (page + 1).to_string();
    let output = run_poppler(
        Command::new("pdftoppm")
            .args([
                "-f",
                &n,
                "-l",
                &n,
                "-singlefile",
                "-aa",
                "yes",
                "-aaVector",
                "yes",
            ])
            .args(["-scale-to-x", &w.to_string(), "-scale-to-y", &h.to_string()])
            .arg(path),
        "pdftoppm",
    )?;
    image::load_from_memory_with_format(&output, image::ImageFormat::Pnm)
        .map_err(|e| anyhow!("{}ページ目を画像にできません: {e}", page + 1))
}

/// popplerの道具を動かし、標準出力を返す。無ければ入れ方を知らせる。
fn run_poppler(command: &mut Command, name: &str) -> Result<Vec<u8>> {
    let output = match command.output() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "PDFの見た目を出すには{name}が要ります（Debian/Ubuntuならsudo apt install poppler-utils）"
        ),
        Err(e) => bail!("{name}を起動できません: {e}"),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first = stderr.lines().next().unwrap_or("").trim();
        bail!("{name}が失敗しました: {first}");
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_ratio() {
        // A4縦を横長の欄へ: 高さで決まる
        assert_eq!(fit(595.0, 842.0, 1000, 500), (353, 500));
        // 横長を縦長の欄へ: 幅で決まる
        assert_eq!(fit(800.0, 400.0, 400, 1000), (400, 200));
    }

    #[test]
    fn page_sizes_are_parsed_with_rotation() {
        let text = "Pages:           2\n\
                    Page    1 size:  594.96 x 841.92 pts (A4)\n\
                    Page    1 rot:   0\n\
                    Page    2 size:  594.96 x 841.92 pts (A4)\n\
                    Page    2 rot:   90\n\
                    File size:       144932 bytes\n";
        assert_eq!(
            parse_page_sizes(text),
            vec![(594.96, 841.92), (841.92, 594.96)]
        );
    }

    #[test]
    fn centered_puts_image_in_the_middle() {
        let picker = Picker::halfblocks();
        let img = DynamicImage::new_rgb8(100, 100);
        // ブロック文字は1桁10画素・1行20画素とみなす → 10桁×5行
        let protocol = picker
            .new_protocol(img, Size::new(40, 20), Resize::Fit(None))
            .unwrap();
        let rect = centered(&protocol, Rect::new(0, 0, 40, 20));
        assert_eq!((rect.width, rect.height), protocol.size().into());
        assert_eq!(rect.x, (40 - rect.width) / 2);
        assert_eq!(rect.y, (20 - rect.height) / 2);
    }

    #[test]
    fn image_file_is_drawn_into_the_area() {
        let dir = std::env::temp_dir().join(format!("folio-gfx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        DynamicImage::new_rgb8(64, 32).save(&path).unwrap();
        let mut v = Visual::open(&path, Format::Image).unwrap();
        assert_eq!(v.page_count(), 1);
        assert_eq!(v.dimensions(), Some((64, 32)));
        let size = v.page(0, Size::new(30, 10)).unwrap().size();
        assert!(size.width <= 30 && size.height <= 10, "{size:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_image_is_reported() {
        let dir = std::env::temp_dir().join(format!("folio-gfx-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        std::fs::write(&path, "not an image").unwrap();
        let err = Visual::open(&path, Format::Image)
            .err()
            .unwrap()
            .to_string();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(err.contains("画像として読めません"), "{err}");
    }

    #[test]
    fn pdf_page_is_rendered() {
        if Command::new("pdftoppm").arg("-v").output().is_err() {
            return; // popplerの無い環境では確かめない
        }
        let dir = std::env::temp_dir().join(format!("folio-gfx-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.pdf");
        std::fs::write(&path, crate::pdf::tests::minimal_pdf("Hi")).unwrap();
        let mut v = Visual::open(&path, Format::Pdf).unwrap();
        assert!(v.is_pdf());
        assert_eq!(v.page_count(), 1);
        assert!(v.page(0, Size::new(20, 10)).is_ok());
        assert!(v.page(1, Size::new(20, 10)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
