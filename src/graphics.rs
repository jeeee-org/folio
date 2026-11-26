//! 画像を端末に描く部品。画像ファイルとPDFのページを、端末の描き方（Kitty方式・sixel、
//! 描けなければブロック文字）に合わせて用意する。端末の問い合わせは起動時に1回だけ。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};
use image::DynamicImage;
use image::ImageDecoder;
use image::imageops::FilterType;
use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui_image::Resize;
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};
use resvg::usvg;

use crate::format::Format;
use crate::render::ImageSlot;

static PICKER: OnceLock<Picker> = OnceLock::new();

/// 覚えておくページの数（行き来した時に描き直さない）。
const CACHE_PAGES: usize = 8;

/// 端末に描き方を問い合わせる。TUIを始めた直後（キーを読む前）に1回だけ呼ぶ。
/// 環境変数`FOLIO_GRAPHICS`（`kitty` / `sixel` / `iterm2` / `halfblocks`）で描き方を決め打ちできる。
/// tmux・screenの中では問い合わせない（下の`in_multiplexer`）。
pub fn init() {
    if PICKER.get().is_some() {
        return;
    }
    let forced = std::env::var("FOLIO_GRAPHICS").ok();
    let query = match forced.as_deref() {
        Some("halfblocks") => false,
        Some(_) => true,
        None => !in_multiplexer(),
    };
    let mut picker = if query {
        let options = QueryStdioOptions {
            timeout: QUERY_TIMEOUT,
            ..QueryStdioOptions::default()
        };
        Picker::from_query_stdio_with_options(options).unwrap_or_else(|_| Picker::halfblocks())
    } else {
        Picker::halfblocks()
    };
    match forced.as_deref() {
        Some("kitty") => picker.set_protocol_type(ProtocolType::Kitty),
        Some("sixel") => picker.set_protocol_type(ProtocolType::Sixel),
        Some("iterm2") => picker.set_protocol_type(ProtocolType::Iterm2),
        _ => {}
    }
    let _ = PICKER.set(picker);
}

/// 問い合わせの答えを待つ長さ。答える端末は数ミリ秒で返す。
const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// tmux・screenの中か。中では問い合わせを外の端末へ包んで送るが、tmuxが
/// `allow-passthrough`を許していないと誰も答えず、答えを待つ読み手が後から打ったキーを
/// 横取りしてしまう（起動直後のキーが効かなくなる）。描くのは次の段階なので、中では問い合わせない。
fn in_multiplexer() -> bool {
    std::env::var_os("TMUX").is_some()
        || std::env::var("TERM").is_ok_and(|t| t.starts_with("tmux") || t.starts_with("screen"))
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
    /// 読み込んだSVG（描く時に欄の大きさで画素にする）
    Svg(Box<usvg::Tree>),
    /// ページごとの大きさ（ポイント。回転を反映した幅・高さ）
    Pdf(Vec<(f64, f64)>),
}

impl Visual {
    /// 画像ファイルかPDFを開く。PDFはページ数と大きさだけを先に読む。
    pub fn open(path: &Path, format: Format) -> Result<Self> {
        let kind = match format {
            Format::Pdf => Kind::Pdf(pdf_page_sizes(path)?),
            _ if is_svg(path) => Kind::Svg(Box::new(load_svg(path)?)),
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
            Kind::Image(_) | Kind::Svg(_) => 1,
            Kind::Pdf(pages) => pages.len(),
        }
    }

    /// 画像の画素数（画像ファイルだけ。SVGは書かれた幅・高さ）。
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        match &self.kind {
            Kind::Image(img) => Some((img.width(), img.height())),
            Kind::Svg(tree) => Some(svg_size(tree)),
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
            Kind::Svg(tree) => {
                // ベクターなので欄いっぱいの大きさで描く（小さなアイコンも拡大してぼやけない）
                let size = tree.size();
                let (pw, ph) = fit(
                    f64::from(size.width()),
                    f64::from(size.height()),
                    max_w,
                    max_h,
                );
                rasterize_svg(tree, pw, ph)?
            }
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

/// 覚えておく本文の中の画像の数。
const CACHE_INLINE: usize = 24;

/// マークダウンの中の画像。パスをmdファイルの場所から解決し、描く大きさを見積もり、
/// 描いたものを覚えておく。スクロールで端にかかった画像は見えている行だけを描く。
pub struct InlineImages {
    /// mdファイルのあるフォルダ
    base: PathBuf,
    /// `src`ごとの画素数（読めなければ`None`）
    dims: HashMap<String, Option<(u32, u32)>>,
    /// 描いたもの（`src`・大きさ・結果）。新しいものほど後ろ
    drawn: Vec<(String, Size, Result<SlicedProtocol, String>)>,
}

impl InlineImages {
    /// `doc`はmdファイルのパス。
    pub fn new(doc: &Path) -> Self {
        let base = doc
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        Self {
            base,
            dims: HashMap::new(),
            drawn: Vec::new(),
        }
    }

    /// 覚えたものを捨てる（ファイルを読み直した時）。
    pub fn clear(&mut self) {
        self.dims.clear();
        self.drawn.clear();
    }

    /// `src`を描く大きさ（桁・行）。`avail`桁・`max_rows`行に収め、元の画素より大きくしない。
    /// 読めない画像は`None`。
    pub fn size(&mut self, src: &str, avail: usize, max_rows: u16) -> Option<(u16, u16)> {
        let (w, h) = self.dimensions(src)?;
        let font = picker().font_size();
        cell_size(
            (w, h),
            (u32::from(font.width), u32::from(font.height)),
            avail,
            max_rows,
        )
    }

    fn dimensions(&mut self, src: &str) -> Option<(u32, u32)> {
        if let Some(d) = self.dims.get(src) {
            return *d;
        }
        let d = resolve_image(&self.base, src).and_then(|p| image_dimensions(&p));
        self.dims.insert(src.to_string(), d);
        d
    }

    /// 本文の`area`に、`scroll`行目から見えている画像を描く。
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, slots: &[ImageSlot], scroll: usize) {
        let bottom = scroll + usize::from(area.height);
        for slot in slots {
            let end = slot.line + usize::from(slot.height);
            if end <= scroll || slot.line >= bottom {
                continue;
            }
            let size = Size::new(slot.width, slot.height);
            let base = self.base.clone();
            let entry = self.entry(&slot.src, size, || {
                let path =
                    resolve_image(&base, &slot.src).ok_or_else(|| "見つかりません".to_string())?;
                let img = if is_svg(&path) {
                    load_svg(&path).and_then(|tree| {
                        let font = picker().font_size();
                        let size = tree.size();
                        let (pw, ph) = fit(
                            f64::from(size.width()),
                            f64::from(size.height()),
                            u32::from(slot.width) * u32::from(font.width),
                            u32::from(slot.height) * u32::from(font.height),
                        );
                        rasterize_svg(&tree, pw, ph)
                    })
                } else {
                    load_image(&path)
                }
                .map_err(|e| format!("{e:#}"))?;
                SlicedProtocol::new_with_resize(
                    picker(),
                    img,
                    size,
                    Resize::Fit(Some(FilterType::Triangle)),
                )
                .map_err(|e| format!("画像にできません: {e}"))
            });
            let y = slot.line as i64 - scroll as i64;
            match entry {
                Ok(protocol) => {
                    let position = SignedPosition {
                        x: slot.col.min(usize::from(area.width)) as i16,
                        y: y.clamp(-i64::from(i16::MAX), i64::from(i16::MAX)) as i16,
                    };
                    frame.render_widget(SlicedImage::new(protocol, position), area);
                }
                Err(e) => {
                    // 見えている最初の行に知らせる
                    let row = area.y + y.max(0) as u16;
                    let x = area.x + (slot.col as u16).min(area.width);
                    let at = Rect::new(x, row, area.right().saturating_sub(x), 1);
                    let text = format!("[画像: {}（{e}）]", slot.src);
                    frame.render_widget(Paragraph::new(Line::from(text).red()), at);
                }
            }
        }
    }

    fn entry(
        &mut self,
        src: &str,
        size: Size,
        make: impl FnOnce() -> Result<SlicedProtocol, String>,
    ) -> &Result<SlicedProtocol, String> {
        if let Some(i) = self
            .drawn
            .iter()
            .position(|(s, z, _)| s == src && *z == size)
        {
            let hit = self.drawn.remove(i);
            self.drawn.push(hit);
        } else {
            let made = make();
            if self.drawn.len() >= CACHE_INLINE {
                drop(self.drawn.remove(0));
            }
            self.drawn.push((src.to_string(), size, made));
        }
        &self.drawn.last().expect("直前に入れた").2
    }
}

/// 画素`(w, h)`の画像を、文字の大きさ`font`（画素）で`avail`桁・`max_rows`行に収める桁数・行数。
/// 元の画素より大きくはしない。
fn cell_size(
    (w, h): (u32, u32),
    (fw, fh): (u32, u32),
    avail: usize,
    max_rows: u16,
) -> Option<(u16, u16)> {
    if w == 0 || h == 0 || fw == 0 || fh == 0 || avail == 0 || max_rows == 0 {
        return None;
    }
    let max_w = (avail as u32).min(u32::from(u16::MAX)) * fw;
    let max_h = u32::from(max_rows) * fh;
    let (pw, ph) = if w <= max_w && h <= max_h {
        (w, h)
    } else {
        fit(f64::from(w), f64::from(h), max_w, max_h)
    };
    let cols = pw.div_ceil(fw).clamp(1, avail as u32);
    let rows = ph.div_ceil(fh).clamp(1, u32::from(max_rows));
    Some((cols as u16, rows as u16))
}

/// マークダウンに書かれた画像のパスを、mdファイルのあるフォルダ`base`から解決する。
/// URL（`http:`など）・`data:`は扱わない。`%20`のような符号はほどく。
pub fn resolve_image(base: &Path, src: &str) -> Option<PathBuf> {
    let src = src.trim();
    let src = src.strip_prefix("file://").unwrap_or(src);
    if src.is_empty() || src.contains("://") || src.starts_with("data:") {
        return None;
    }
    let decoded = percent_decode(src);
    let path = Path::new(&decoded);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    path.is_file().then_some(path)
}

/// `%XX`をほどく。UTF-8として読めなければ元のまま。
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// 画像の画素数を、中身を全部は読まずに調べる（写真の向きを反映する）。
fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    if is_svg(path) {
        return load_svg(path).ok().map(|tree| svg_size(&tree));
    }
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    let (w, h) = decoder.dimensions();
    let swap = decoder.orientation().is_ok_and(|o| {
        use image::metadata::Orientation::*;
        matches!(o, Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH)
    });
    Some(if swap { (h, w) } else { (w, h) })
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

/// 拡張子がSVG（`.svg`・`.svgz`）か。
fn is_svg(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"))
}

/// このPCのフォント。SVGの中の文字を描くのに使う。最初のSVGを開いた時に1回だけ読む。
fn fonts() -> std::sync::Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<std::sync::Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            set_generic_families(&mut db);
            std::sync::Arc::new(db)
        })
        .clone()
}

/// `sans-serif`のような総称のフォント名に、このPCにあるフォントを当てる。fontdbの既定は
/// ArialやTimes New Romanで、無いPCでは文字が1つも描かれない。fontconfigの選び方（日本語）に
/// 従い、`fc-match`が無ければ候補から、それも無ければ最初に見つかったフォントにする。
fn set_generic_families(db: &mut usvg::fontdb::Database) {
    let names: Vec<String> = db
        .faces()
        .flat_map(|f| f.families.iter().map(|(name, _)| name.clone()))
        .collect();
    let Some(first) = names.first().cloned() else {
        return; // フォントが1つも無い
    };
    let pick = |generic: &str, candidates: &[&str]| -> String {
        let matched = Command::new("fc-match")
            .args(["-f", "%{family[0]}", &format!("{generic}:lang=ja")])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok());
        matched
            .into_iter()
            .chain(candidates.iter().map(|c| c.to_string()))
            .find(|name| names.contains(name))
            .unwrap_or_else(|| first.clone())
    };
    const SANS: &[&str] = &[
        "Noto Sans CJK JP",
        "Noto Sans JP",
        "IPAexGothic",
        "IPAGothic",
        "DejaVu Sans",
        "Liberation Sans",
    ];
    let sans = pick("sans-serif", SANS);
    let serif = pick(
        "serif",
        &[
            "Noto Serif CJK JP",
            "Noto Serif JP",
            "IPAexMincho",
            "DejaVu Serif",
            "Liberation Serif",
        ],
    );
    let mono = pick(
        "monospace",
        &[
            "Noto Sans Mono CJK JP",
            "DejaVu Sans Mono",
            "Liberation Mono",
        ],
    );
    db.set_serif_family(serif);
    db.set_monospace_family(mono);
    db.set_cursive_family(sans.clone());
    db.set_fantasy_family(sans.clone());
    db.set_sans_serif_family(sans);
}

/// SVGファイルを読む（`.svgz`は自動でほどける）。中で参照する画像はSVGのあるフォルダから探す。
fn load_svg(path: &Path) -> Result<usvg::Tree> {
    let data = std::fs::read(path).with_context(|| format!("{}を読めません", path.display()))?;
    let options = usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        fontdb: fonts(),
        ..usvg::Options::default()
    };
    usvg::Tree::from_data(&data, &options)
        .map_err(|e| anyhow!("{}はSVGとして読めません: {e}", path.display()))
}

/// SVGに書かれた幅・高さ（画素。端数は切り上げ）。
fn svg_size(tree: &usvg::Tree) -> (u32, u32) {
    let size = tree.size();
    (
        (size.width().ceil() as u32).max(1),
        (size.height().ceil() as u32).max(1),
    )
}

/// SVGを幅`w`・高さ`h`の画素に描く。透明なところは白で埋める（暗い端末で黒い線が消えないように）。
fn rasterize_svg(tree: &usvg::Tree, w: u32, h: u32) -> Result<DynamicImage> {
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(w, h).ok_or_else(|| anyhow!("SVGを描く場所がありません"))?;
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    let size = tree.size();
    let transform =
        resvg::tiny_skia::Transform::from_scale(w as f32 / size.width(), h as f32 / size.height());
    resvg::render(tree, transform, &mut pixmap.as_mut());
    // 白で埋めたので不透明。乗算済みの色をそのまま使える
    let image = image::RgbaImage::from_raw(w, h, pixmap.take())
        .ok_or_else(|| anyhow!("SVGを画像にできません"))?;
    Ok(DynamicImage::ImageRgba8(image))
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

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20" viewBox="0 0 4 2"><rect x="0" y="0" width="2" height="2" fill="#f00"/></svg>"##;

    #[test]
    fn svg_file_is_drawn_into_the_area_on_white() {
        let dir = std::env::temp_dir().join(format!("folio-gfx-svg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.svg");
        std::fs::write(&path, SVG).unwrap();
        let mut v = Visual::open(&path, Format::Image).unwrap();
        assert_eq!(v.page_count(), 1);
        assert_eq!(v.dimensions(), Some((40, 20)));
        let size = v.page(0, Size::new(30, 10)).unwrap().size();
        assert!(size.width <= 30 && size.height <= 10, "{size:?}");
        // 左半分は赤、右半分は透明だったところが白
        let Kind::Svg(tree) = &v.kind else {
            panic!("SVGとして開いていない")
        };
        let img = rasterize_svg(tree, 80, 40).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(10, 20).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(70, 20).0, [255, 255, 255, 255]);
        // マークダウンの中から参照した時も大きさが分かる
        assert_eq!(image_dimensions(&path), Some((40, 20)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_svg_is_reported() {
        let dir = std::env::temp_dir().join(format!("folio-gfx-svg-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.svg");
        std::fs::write(&path, "<svg").unwrap();
        let err = Visual::open(&path, Format::Image)
            .err()
            .unwrap()
            .to_string();
        assert!(image_dimensions(&path).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(err.contains("SVGとして読めません"), "{err}");
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

    #[test]
    fn inline_image_size_fits_width_and_height_without_enlarging() {
        // 文字は10×20画素とする
        // 小さい画像は元の大きさ（端数は切り上げ）
        assert_eq!(cell_size((95, 41), (10, 20), 80, 30), Some((10, 3)));
        // 幅で縮める
        assert_eq!(cell_size((1600, 400), (10, 20), 80, 30), Some((80, 10)));
        // 高さで縮める
        assert_eq!(cell_size((400, 2000), (10, 20), 80, 25), Some((10, 25)));
        assert_eq!(cell_size((400, 2000), (10, 20), 80, 0), None);
    }

    #[test]
    fn inline_image_path_is_resolved_from_the_document_folder() {
        let dir = std::env::temp_dir().join(format!("folio-gfx-md-{}", std::process::id()));
        let docs = dir.join("notes/sub");
        let figs = dir.join("trials/fig a");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::create_dir_all(&figs).unwrap();
        let png = figs.join("5-点と費用.png");
        DynamicImage::new_rgb8(30, 20).save(&png).unwrap();
        let doc = docs.join("a.md");
        let images = InlineImages::new(&doc);
        let found = resolve_image(&images.base, "../../trials/fig a/5-点と費用.png");
        assert_eq!(
            found.map(|p| p.canonicalize().unwrap()),
            Some(png.canonicalize().unwrap())
        );
        // 符号化されたパス・絶対パス
        assert!(
            resolve_image(
                &images.base,
                "../../trials/fig%20a/5-%E7%82%B9%E3%81%A8%E8%B2%BB%E7%94%A8.png"
            )
            .is_some()
        );
        assert!(resolve_image(&images.base, png.to_str().unwrap()).is_some());
        // 無いもの・URL
        assert!(resolve_image(&images.base, "../nothing.png").is_none());
        assert!(resolve_image(&images.base, "https://example.com/a.png").is_none());
        let mut images = images;
        assert_eq!(
            images.dimensions("../../trials/fig a/5-点と費用.png"),
            Some((30, 20))
        );
        // 相対パスのmdファイル名（フォルダが空）でも今のフォルダから探す
        assert_eq!(InlineImages::new(Path::new("a.md")).base, Path::new("."));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
