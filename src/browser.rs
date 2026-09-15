//! `folio browse`。左にディレクトリ一覧、右にプレビューの2ペインのファイラー（第一歩）。
//! ファイル操作はしない。ターミナル制御はこのモジュールに閉じ込め、プレビューの描画は
//! ビューアーと同じ中核（format / render）を使う。

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use unicode_width::UnicodeWidthStr;

use crate::config::Config;
use crate::fileops;
use crate::format::Format;
use crate::highlight::Highlighter;
use crate::render::{self, Flow, Options, Rendered, Theme};
use crate::search;
use crate::select::{self, Dragger};
use crate::viewer;

/// プレビューで読む上限（バイト）。これより大きいファイルは読まない。
pub const PREVIEW_MAX_BYTES: u64 = 1024 * 1024;

/// 同じ項目への2回のクリックをダブルクリックとみなす間隔。
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// 一覧の1項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

impl Entry {
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

/// ディレクトリの中身を読み、ディレクトリを先に・名前の自然順で並べる。
pub fn list_dir(dir: &Path, show_hidden: bool) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(dir).with_context(|| format!("{}を読めません", dir.display()))? {
        let item = match item {
            Ok(i) => i,
            Err(_) => continue,
        };
        let name = item.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        // シンボリックリンクはリンク先で判定する（読めなければファイル扱い）
        let is_dir = fs::metadata(item.path())
            .map(|m| m.is_dir())
            .unwrap_or(false);
        entries.push(Entry {
            name,
            path: item.path(),
            is_dir,
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
    Ok(entries)
}

/// 数字の並びを数として比べる自然順（`file2` < `file10`）。大文字小文字は区別しない。
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let mut na = 0u128;
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    na = na * 10 + (c as u128 - '0' as u128);
                    ai.next();
                }
                let mut nb = 0u128;
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    nb = nb * 10 + (c as u128 - '0' as u128);
                    bi.next();
                }
                if na != nb {
                    return na.cmp(&nb);
                }
            }
            (Some(ca), Some(cb)) => {
                let (la, lb) = (
                    ca.to_lowercase().next().unwrap_or(ca),
                    cb.to_lowercase().next().unwrap_or(cb),
                );
                if la != lb {
                    return la.cmp(&lb);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// プレビューに使う内容。読まない理由もここで決める。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewSource {
    Text(String),
    Skip(String),
}

/// ファイルをプレビュー用に読む。大きい・UTF-8でない時は理由を返して読まない。
pub fn read_for_preview(path: &Path) -> PreviewSource {
    let size = match fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) => return PreviewSource::Skip(format!("読めません: {e}")),
    };
    if size > PREVIEW_MAX_BYTES {
        return PreviewSource::Skip(format!(
            "プレビューなし（{}。{}を超える）",
            human_size(size),
            human_size(PREVIEW_MAX_BYTES)
        ));
    }
    match fs::read(path) {
        Err(e) => PreviewSource::Skip(format!("読めません: {e}")),
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => PreviewSource::Text(text),
            Err(_) => PreviewSource::Skip("プレビューなし（UTF-8ではない）".to_string()),
        },
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes}B")
    } else {
        format!("{v:.1}{}", UNITS[i])
    }
}

/// `folio browse [dir]`の本体。終了時にいたディレクトリを返す。
pub fn run(start: &Path, config: &Config) -> Result<PathBuf> {
    let start =
        fs::canonicalize(start).with_context(|| format!("{}が見つかりません", start.display()))?;
    let mut terminal = ratatui::init();
    let result = viewer::set_mouse_capture(true)
        .and_then(|()| Browser::new(start, config))
        .and_then(|mut b| b.run(&mut terminal));
    let _ = viewer::set_mouse_capture(false);
    ratatui::restore();
    result
}

/// 一覧の中身の領域`inner`で、画面の位置`at`にある項目の添字。`offset`は一覧の表示の先頭。
fn list_index_at(inner: Rect, offset: usize, len: usize, at: Position) -> Option<usize> {
    if !inner.contains(at) {
        return None;
    }
    let index = offset + (at.y - inner.y) as usize;
    (index < len).then_some(index)
}

/// 直前のクリック`prev`（時刻と項目）に続けて`index`をクリックしたらダブルクリックか。
fn is_double_click(prev: Option<(Instant, usize)>, now: Instant, index: usize) -> bool {
    prev.is_some_and(|(at, i)| i == index && now.saturating_duration_since(at) <= DOUBLE_CLICK)
}

/// 右ペインの状態。カーソルの項目が変わった時だけ作り直す。
struct Preview {
    path: PathBuf,
    width: u16,
    lines: Vec<Line<'static>>,
    flow: Vec<Flow>,
    scroll: usize,
    /// フォルダの中身のプレビューなら、その項目（行と同じ順）。ファイルなら空
    children: Vec<Entry>,
}

struct Browser<'c> {
    config: &'c Config,
    theme: Theme,
    highlighter: Highlighter,
    cwd: PathBuf,
    entries: Vec<Entry>,
    cursor: usize,
    show_hidden: bool,
    preview: Option<Preview>,
    message: Option<String>,
    pending_g: bool,
    list_height: usize,
    quit: bool,
    /// 下部の1行で受けている入力・確認
    prompt: Option<Prompt>,
    /// `y`/`x`で覚えた項目
    clip: Option<Clip>,
    /// 一覧の表示位置（スクロール）。クリックの行から項目を求めるのに使う
    list_state: ListState,
    /// 直前の描画での一覧（上端はパスの行）・一覧の中身・プレビューの領域
    list_area: Rect,
    list_inner: Rect,
    preview_area: Rect,
    /// 直前のクリック（ダブルクリックの判定用）
    last_click: Option<(Instant, usize)>,
    /// 直前の描画でのプレビューの本文の領域（選択の位置を求める）
    preview_inner: Rect,
    /// プレビューでのドラッグ選択
    drag: Dragger,
}

/// コピー・移動の元。
#[derive(Debug, Clone)]
struct Clip {
    path: PathBuf,
    name: String,
    cut: bool,
}

/// 下部の1行で受ける入力と確認。
enum Prompt {
    /// 新規作成の名前
    Create { input: String },
    /// 改名。`path`が対象
    Rename { path: PathBuf, input: String },
    /// ごみ箱へ移す確認。`y`だけが実行
    Trash { path: PathBuf, name: String },
    /// 貼り付け先に同名がある時の上書き確認。`y`だけが実行
    Overwrite { clip: Clip },
}

impl<'c> Browser<'c> {
    fn new(cwd: PathBuf, config: &'c Config) -> Result<Self> {
        let mut b = Self {
            config,
            theme: config.theme()?,
            highlighter: Highlighter::with_theme(&config.highlight.theme),
            cwd,
            entries: Vec::new(),
            cursor: 0,
            show_hidden: false,
            preview: None,
            message: None,
            pending_g: false,
            list_height: 1,
            quit: false,
            prompt: None,
            clip: None,
            list_state: ListState::default(),
            list_area: Rect::default(),
            list_inner: Rect::default(),
            preview_area: Rect::default(),
            last_click: None,
            preview_inner: Rect::default(),
            drag: Dragger::default(),
        };
        b.reload(None);
        Ok(b)
    }

    /// 一覧を読み直す。`select`があればその名前の項目にカーソルを置く。
    fn reload(&mut self, select: Option<&str>) {
        match list_dir(&self.cwd, self.show_hidden) {
            Ok(entries) => {
                self.entries = entries;
                self.message = None;
            }
            Err(err) => {
                self.entries = Vec::new();
                self.message = Some(err.to_string());
            }
        }
        self.cursor = select
            .and_then(|name| self.entries.iter().position(|e| e.name == name))
            .unwrap_or(0)
            .min(self.entries.len().saturating_sub(1));
        self.preview = None;
        *self.list_state.offset_mut() = 0;
        self.last_click = None;
    }

    fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<PathBuf> {
        let mut dirty = true;
        while !self.quit {
            if dirty {
                terminal.draw(|frame| self.draw(frame))?;
            }
            dirty = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    self.key(terminal, key)?;
                    true
                }
                // ポインタを動かしただけの時などは描き直さない
                Event::Mouse(m) => self.mouse(terminal, m)?,
                _ => true,
            };
        }
        Ok(self.cwd.clone())
    }

    /// マウス。1回のクリックで選び、同じ項目をもう1回で開く。パスの行のクリックで親へ。
    /// ホイールはポインタの下の欄を動かす。画面が変わる時に`true`。入力・確認中は無視する。
    fn mouse(&mut self, terminal: &mut DefaultTerminal, m: MouseEvent) -> Result<bool> {
        if self.prompt.is_some() {
            return Ok(false);
        }
        let at = Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let step = if m.kind == MouseEventKind::ScrollDown {
                    viewer::WHEEL_STEP
                } else {
                    -viewer::WHEEL_STEP
                };
                if self.preview_area.contains(at) {
                    self.scroll_preview(step);
                } else if self.list_area.contains(at) {
                    self.move_cursor(step);
                } else {
                    return Ok(false);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.drag.clear();
                if self.preview_inner.contains(at) {
                    self.message = None;
                    self.last_click = None;
                    let listing = self
                        .preview
                        .as_ref()
                        .is_some_and(|p| !p.children.is_empty());
                    if listing {
                        // フォルダの中身: クリックした項目を1回で開く
                        if let Some(child) = self.preview_child_at(at) {
                            self.open_child(terminal, child)?;
                        }
                    } else if let Some(p) = self.preview_point(at) {
                        // ファイルの中身: 文字を選び始める
                        self.drag.begin(p);
                    }
                } else if self.list_area.contains(at) && at.y == self.list_area.y {
                    self.message = None;
                    self.go_parent();
                } else if let Some(index) = list_index_at(
                    self.list_inner,
                    self.list_state.offset(),
                    self.entries.len(),
                    at,
                ) {
                    let now = Instant::now();
                    self.message = None;
                    self.pending_g = false;
                    self.set_cursor(index);
                    if is_double_click(self.last_click, now, index) {
                        self.last_click = None;
                        self.open(terminal)?;
                    } else {
                        self.last_click = Some((now, index));
                    }
                } else {
                    return Ok(false);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.drag.is_dragging() => {
                // プレビューの上下にはみ出したら、その向きに1行ずつ送る
                self.scroll_preview(select::edge_scroll(self.preview_inner, at.y));
                if let Some(p) = self.preview_point(at) {
                    self.drag.extend(p);
                }
            }
            MouseEventKind::Up(MouseButton::Left) if self.drag.is_dragging() => {
                if self.drag.finish().is_some() {
                    self.message = Some(viewer::SELECTED_HINT.to_string());
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// プレビューがフォルダの中身の時、画面の位置の行にある項目。
    fn preview_child_at(&self, at: Position) -> Option<Entry> {
        let p = self.preview.as_ref()?;
        let row = at.y.checked_sub(self.preview_inner.y)? as usize;
        p.children.get(p.scroll + row).cloned()
    }

    /// プレビューに出ているフォルダの中身の項目を開く。そのフォルダへ移って項目にカーソルを置き、
    /// 一覧で開く時と同じく、フォルダなら中へ入り、ファイルなら全画面で読む。
    fn open_child(&mut self, terminal: &mut DefaultTerminal, child: Entry) -> Result<()> {
        let Some(parent) = child.path.parent().map(Path::to_path_buf) else {
            return Ok(());
        };
        self.cwd = parent;
        self.reload(Some(&child.name));
        if self.current().is_some_and(|e| e.name == child.name) {
            self.open(terminal)?;
        }
        Ok(())
    }

    /// 画面の位置を、プレビューの行の中の位置に直す。
    fn preview_point(&self, at: Position) -> Option<select::Point> {
        let p = self.preview.as_ref()?;
        select::point_at(self.preview_inner, p.scroll, p.lines.len(), at.x, at.y)
    }

    fn key(&mut self, terminal: &mut DefaultTerminal, key: KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.prompt.is_some() {
            self.prompt_key(key);
            return Ok(());
        }
        self.message = None;
        // プレビューで選んでいれば、Ctrl-cはコピー・Escは解除（終了や忘れるより先）
        if let Some(p) = &self.preview
            && viewer::selection_key(&mut self.drag, key, &mut self.message, |s| {
                select::text(&p.lines, &p.flow, s)
            })?
        {
            return Ok(());
        }
        self.drag.clear();
        if self.pending_g {
            self.pending_g = false;
            if key.code == KeyCode::Char('g') {
                self.cursor = 0;
                self.preview = None;
                return Ok(());
            }
        }
        let half = (self.list_height / 2).max(1);
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            // Escは覚えた項目を忘れる。何も無ければ終了
            KeyCode::Esc if self.clip.is_some() => self.clip = None,
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('y') => self.remember(false),
            KeyCode::Char('x') => self.remember(true),
            KeyCode::Char('p') => self.paste(),
            KeyCode::Char('o') => {
                if let Some(e) = self.current().cloned() {
                    self.message = Some(match fileops::open_external(&e.path) {
                        Ok(cmd) => format!("{cmd}で開きました: {}", e.name),
                        Err(err) => format!("開けません: {err:#}"),
                    });
                }
            }
            KeyCode::Char('d') if ctrl => self.move_cursor(half as isize),
            KeyCode::Char('u') if ctrl => self.move_cursor(-(half as isize)),
            KeyCode::PageDown => self.move_cursor(half as isize),
            KeyCode::PageUp => self.move_cursor(-(half as isize)),
            KeyCode::Char('J') => self.scroll_preview(10),
            KeyCode::Char('K') => self.scroll_preview(-10),
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Char('G') | KeyCode::End => self.set_cursor(usize::MAX),
            KeyCode::Home => self.set_cursor(0),
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => self.go_parent(),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => self.open(terminal)?,
            KeyCode::Char('e') => self.edit(terminal)?,
            KeyCode::Char('.') => {
                self.show_hidden = !self.show_hidden;
                let keep = self.current().map(|e| e.name.clone());
                self.reload(keep.as_deref());
            }
            KeyCode::Char('R') => {
                let keep = self.current().map(|e| e.name.clone());
                self.reload(keep.as_deref());
            }
            // ファイル操作（yaziと同じキー）
            KeyCode::Char('a') => {
                self.prompt = Some(Prompt::Create {
                    input: String::new(),
                })
            }
            KeyCode::Char('r') => {
                if let Some(e) = self.current().cloned() {
                    self.prompt = Some(Prompt::Rename {
                        path: e.path,
                        input: e.name,
                    });
                }
            }
            KeyCode::Char('d') => {
                if let Some(e) = self.current().cloned() {
                    self.prompt = Some(Prompt::Trash {
                        path: e.path,
                        name: e.name,
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// 入力・確認中のキー。`Enter`で実行、`Esc`で中止。確認は`y`だけが実行。
    fn prompt_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.prompt = None;
            return;
        }
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        match prompt {
            Prompt::Trash { .. } | Prompt::Overwrite { .. } => {
                let confirmed = matches!(key.code, KeyCode::Char('y' | 'Y'));
                let prompt = self.prompt.take().unwrap();
                if !confirmed {
                    self.message = Some("中止しました".to_string());
                    return;
                }
                match prompt {
                    Prompt::Trash { path, name } => self.trash(&path, &name),
                    Prompt::Overwrite { clip } => self.paste_clip(&clip, true),
                    _ => {}
                }
            }
            Prompt::Create { input } | Prompt::Rename { input, .. } => match key.code {
                KeyCode::Char(c) => input.push(c),
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Esc => {
                    self.prompt = None;
                    self.message = Some("中止しました".to_string());
                }
                KeyCode::Enter => {
                    let prompt = self.prompt.take().unwrap();
                    match prompt {
                        Prompt::Create { input } => self.create(&input),
                        Prompt::Rename { path, input } => self.rename(&path, &input),
                        Prompt::Trash { .. } | Prompt::Overwrite { .. } => {}
                    }
                }
                _ => {}
            },
        }
    }

    /// `y`（コピー）/`x`（移動）で選択中の項目を覚える。
    fn remember(&mut self, cut: bool) {
        if let Some(e) = self.current().cloned() {
            self.clip = Some(Clip {
                path: e.path,
                name: e.name.clone(),
                cut,
            });
            self.message = Some(format!(
                "{}を覚えました: {}（pで貼り付け、Escで忘れる）",
                if cut { "移動元" } else { "コピー元" },
                e.name
            ));
        }
    }

    /// `p`。同名があれば上書きの確認を挟む。
    fn paste(&mut self) {
        let Some(clip) = self.clip.clone() else {
            self.message = Some("yかxで先に項目を覚えてください".to_string());
            return;
        };
        if self.cwd.join(&clip.name).symlink_metadata().is_ok() {
            self.prompt = Some(Prompt::Overwrite { clip });
        } else {
            self.paste_clip(&clip, false);
        }
    }

    fn paste_clip(&mut self, clip: &Clip, overwrite: bool) {
        let result = if clip.cut {
            fileops::move_into(&clip.path, &self.cwd, overwrite)
        } else {
            fileops::copy_into(&clip.path, &self.cwd, overwrite)
        };
        match result {
            Ok(target) => {
                let name = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if clip.cut {
                    self.clip = None;
                }
                self.reload(Some(&name));
                self.message = Some(format!(
                    "{}: {}",
                    if clip.cut {
                        "移動しました"
                    } else {
                        "コピーしました"
                    },
                    name
                ));
            }
            Err(err) => self.message = Some(format!("{err:#}")),
        }
    }

    fn create(&mut self, name: &str) {
        match fileops::create(&self.cwd, name) {
            Ok(path) => {
                // 途中のディレクトリを作った時は、その最初の要素にカーソルを置く
                let first = name
                    .trim_start_matches('/')
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .to_string();
                self.reload(Some(&first));
                self.message = Some(format!("作りました: {}", path.display()));
            }
            Err(err) => self.message = Some(format!("作れません: {err:#}")),
        }
    }

    fn rename(&mut self, path: &Path, new_name: &str) {
        match fileops::rename(path, new_name) {
            Ok(target) => {
                let name = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.reload(Some(&name));
                self.message = Some(format!("改名しました: {name}"));
            }
            Err(err) => self.message = Some(format!("改名できません: {err:#}")),
        }
    }

    fn trash(&mut self, path: &Path, name: &str) {
        match fileops::trash(path) {
            Ok(dest) => {
                let index = self.cursor;
                self.reload(None);
                // 消した位置に留まる（末尾なら1つ上）
                self.cursor = index.min(self.entries.len().saturating_sub(1));
                self.preview = None;
                self.message = Some(format!("ごみ箱へ移しました: {name} → {}", dest.display()));
            }
            Err(err) => self.message = Some(format!("{err:#}")),
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        self.set_cursor(self.cursor.saturating_add_signed(delta));
    }

    fn set_cursor(&mut self, to: usize) {
        let new = to.min(self.entries.len().saturating_sub(1));
        if new != self.cursor {
            self.cursor = new;
            self.preview = None;
        }
    }

    fn go_parent(&mut self) {
        let Some(parent) = self.cwd.parent().map(Path::to_path_buf) else {
            return;
        };
        let came_from = self
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        self.cwd = parent;
        self.reload(came_from.as_deref());
    }

    /// ディレクトリなら入る。ファイルならビューアーを全画面で開き、`q`で戻る。
    fn open(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let Some(entry) = self.current().cloned() else {
            return Ok(());
        };
        if entry.is_dir {
            match list_dir(&entry.path, self.show_hidden) {
                Ok(_) => {
                    self.cwd = entry.path;
                    self.reload(None);
                }
                Err(err) => self.message = Some(err.to_string()),
            }
            return Ok(());
        }
        let format = Format::detect(&entry.path, |e| self.highlighter.supports(e));
        if let Err(err) = viewer::view_in(terminal, &entry.path, self.config, Some(format)) {
            terminal.clear()?;
            self.message = Some(format!("開けません: {err:#}"));
        }
        Ok(())
    }

    fn edit(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let Some(entry) = self.current().cloned() else {
            return Ok(());
        };
        if entry.is_dir {
            return Ok(());
        }
        match viewer::open_editor(terminal, &entry.path)? {
            Ok(()) => self.preview = None, // 編集後は読み直す
            Err(message) => self.message = Some(message),
        }
        Ok(())
    }

    fn scroll_preview(&mut self, delta: isize) {
        let height = self.preview_inner.height as usize;
        if let Some(p) = &mut self.preview {
            p.scroll = p
                .scroll
                .saturating_add_signed(delta)
                .min(p.lines.len().saturating_sub(height));
        }
    }

    // ---- 描画 ----

    fn draw(&mut self, frame: &mut Frame) {
        let [main, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        let list_width = (main.width / 3).clamp(24.min(main.width), 60);
        let [list, preview] =
            Layout::horizontal([Constraint::Length(list_width), Constraint::Fill(1)]).areas(main);
        self.list_height = list.height.max(1) as usize;
        self.list_area = list;
        self.preview_area = preview;
        self.draw_list(frame, list);
        self.draw_preview(frame, preview);
        self.draw_status(frame, status);
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = if self.entries.is_empty() {
            vec![ListItem::new("（空）").dim()]
        } else {
            self.entries
                .iter()
                .map(|e| {
                    let mut style = Style::default();
                    if e.is_dir {
                        style = style.fg(Color::Blue).add_modifier(Modifier::BOLD);
                    }
                    if e.is_hidden() {
                        style = style.add_modifier(Modifier::DIM);
                    }
                    let name = if e.is_dir {
                        format!("{}/", e.name)
                    } else {
                        e.name.clone()
                    };
                    ListItem::new(Span::styled(name, style))
                })
                .collect()
        };
        let title = format!(" {} ", shorten_home(&self.cwd));
        let block = Block::new().borders(Borders::RIGHT).title(title);
        self.list_inner = block.inner(area);
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▸");
        // 表示位置を持ち越す（毎回0から数えると、クリックした行と項目が合わなくなる）
        self.list_state
            .select((!self.entries.is_empty()).then_some(self.cursor));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_preview(&mut self, frame: &mut Frame, area: Rect) {
        let inner = Rect {
            x: area.x + 1,
            width: area.width.saturating_sub(1),
            ..area
        };
        self.preview_inner = inner;
        self.ensure_preview(inner.width);
        let selection = self.drag.selection();
        let Some(p) = &mut self.preview else {
            return;
        };
        let height = inner.height as usize;
        let max_scroll = p.lines.len().saturating_sub(height);
        // 末尾を越えた分は捨てる（越えたまま持つと、戻すのに余分に回すことになる）
        p.scroll = p.scroll.min(max_scroll);
        let scroll = p.scroll;
        let end = (scroll + height).min(p.lines.len());
        let lines: Vec<Line<'static>> = (scroll..end)
            .map(|i| {
                match selection.and_then(|s| select::chars_in_line(&p.lines[i], &p.flow, s, i)) {
                    Some((from, to)) => {
                        search::highlight(&p.lines[i], &[(from, to, select::style())])
                    }
                    None => p.lines[i].clone(),
                }
            })
            .collect();
        let text = Text::from(lines);
        frame.render_widget(Paragraph::new(text), inner);
    }

    /// カーソルの項目か幅が変わっていたらプレビューを作り直す。
    fn ensure_preview(&mut self, width: u16) {
        let Some(entry) = self.current().cloned() else {
            self.preview = None;
            return;
        };
        if let Some(p) = &self.preview
            && p.path == entry.path
            && p.width == width
        {
            return;
        }
        let scroll = self
            .preview
            .as_ref()
            .filter(|p| p.path == entry.path)
            .map(|p| p.scroll)
            .unwrap_or(0);
        let (Rendered { lines, flow, .. }, children) = self.build_preview(&entry, width);
        // 描き直すと行の位置が変わるので、選択は捨てる
        self.drag.clear();
        self.preview = Some(Preview {
            path: entry.path,
            width,
            lines,
            flow,
            scroll,
            children,
        });
    }

    /// プレビューの行と、フォルダならその中身の項目（行と同じ順）。
    fn build_preview(&self, entry: &Entry, width: u16) -> (Rendered, Vec<Entry>) {
        if entry.is_dir {
            return match list_dir(&entry.path, self.show_hidden) {
                Ok(entries) if entries.is_empty() => (
                    lines_only(vec![Line::from("（空のディレクトリ）").dim()]),
                    Vec::new(),
                ),
                Ok(entries) => {
                    let lines = entries
                        .iter()
                        .map(|e| {
                            if e.is_dir {
                                Line::from(Span::styled(
                                    format!("{}/", e.name),
                                    Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
                                ))
                            } else {
                                Line::from(e.name.clone())
                            }
                        })
                        .collect();
                    (lines_only(lines), entries)
                }
                Err(err) => (
                    lines_only(vec![Line::from(err.to_string()).red()]),
                    Vec::new(),
                ),
            };
        }
        match read_for_preview(&entry.path) {
            PreviewSource::Skip(reason) => (lines_only(vec![Line::from(reason).dim()]), Vec::new()),
            PreviewSource::Text(source) => {
                let format = Format::detect(&entry.path, |e| self.highlighter.supports(e));
                let blocks = format.parse(&entry.path, &source);
                let rendered = render::render_with(
                    &blocks,
                    width.saturating_sub(2),
                    &self.theme,
                    Some(&self.highlighter),
                    &Options::default(),
                );
                (rendered, Vec::new())
            }
        }
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let left = match (&self.prompt, &self.message) {
            (Some(Prompt::Create { input }), _) => {
                format!(" 新規作成（末尾/でディレクトリ）: {input}_")
            }
            (Some(Prompt::Rename { input, .. }), _) => format!(" 改名: {input}_"),
            (Some(Prompt::Trash { name, .. }), _) => format!(" ごみ箱へ移す: {name} (y/N)"),
            (Some(Prompt::Overwrite { clip }), _) => {
                format!(" {}は既にあります。上書きしますか (y/N)", clip.name)
            }
            (None, Some(m)) => format!(" {m}"),
            (None, None) if self.clip.is_some() => {
                let c = self.clip.as_ref().unwrap();
                format!(
                    " {}/{}  [{}: {}]",
                    if self.entries.is_empty() {
                        0
                    } else {
                        self.cursor + 1
                    },
                    self.entries.len(),
                    if c.cut { "移動元" } else { "コピー元" },
                    c.name
                )
            }
            (None, None) => format!(
                " {}/{}{}",
                if self.entries.is_empty() {
                    0
                } else {
                    self.cursor + 1
                },
                self.entries.len(),
                if self.show_hidden {
                    "  隠し表示"
                } else {
                    ""
                }
            ),
        };
        let right = match &self.prompt {
            Some(Prompt::Trash { .. } | Prompt::Overwrite { .. }) => "y:実行  それ以外:中止 ",
            Some(_) => "Enter:確定  Esc:中止 ",
            None => {
                "Enter:読む  e:編集  o:外部  a:作成  r:改名  d:ごみ箱  y/x/p:コピー/移動  q:終了 "
            }
        };
        // 入らない時は案内を省き、メッセージや入力を優先する
        let width = area.width as usize;
        let right = if left.width() + right.width() > width {
            ""
        } else {
            right
        };
        let left = truncate_display(&left, width.saturating_sub(right.width()));
        let gap = (area.width as usize).saturating_sub(left.width() + right.width());
        let line = Line::from(vec![
            Span::raw(left),
            Span::raw(" ".repeat(gap)),
            Span::raw(right).dim(),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(Style::new().bg(Color::Indexed(238)).fg(Color::White)),
            area,
        );
    }
}

/// 行どうしのつながりを持たない行の列（ディレクトリの中身や、読まない理由）。
fn lines_only(lines: Vec<Line<'static>>) -> Rendered {
    Rendered {
        lines,
        ..Rendered::default()
    }
}

/// 表示幅で切る（切った印に`…`）。
fn truncate_display(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > width {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// `$HOME`を`~`に縮める（表示用）。
fn shorten_home(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_of_directory_maps_rows_to_children() {
        let dir = std::env::temp_dir().join(format!("folio-preview-click-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub/inner")).unwrap();
        fs::write(dir.join("sub/a.md"), "# a").unwrap();
        fs::write(dir.join("top.md"), "# top").unwrap();
        let config = Config::default();
        let mut b = Browser::new(dir.clone(), &config).unwrap();
        // カーソルは先頭のsub/。右にその中身（inner/、a.md）が出る
        b.preview_inner = Rect::new(40, 0, 40, 10);
        b.ensure_preview(40);
        let at = |y| b.preview_child_at(Position::new(45, y)).map(|e| e.name);
        assert_eq!(at(0).as_deref(), Some("inner"));
        assert_eq!(at(1).as_deref(), Some("a.md"));
        assert_eq!(at(2), None);
        // ファイルのプレビューには項目が無い（ドラッグ選択に回る）
        b.set_cursor(1);
        b.ensure_preview(40);
        assert!(b.preview.as_ref().unwrap().children.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn click_row_maps_to_entry_with_offset() {
        // 一覧の中身が2行目から5行（パスの行が1行目）、表示の先頭が10件目
        let inner = Rect::new(0, 1, 30, 5);
        assert_eq!(list_index_at(inner, 10, 100, Position::new(3, 1)), Some(10));
        assert_eq!(list_index_at(inner, 10, 100, Position::new(3, 5)), Some(14));
        // パスの行・中身の外・項目の無い行
        assert_eq!(list_index_at(inner, 10, 100, Position::new(3, 0)), None);
        assert_eq!(list_index_at(inner, 10, 100, Position::new(30, 2)), None);
        assert_eq!(list_index_at(inner, 0, 2, Position::new(3, 3)), None);
    }

    #[test]
    fn double_click_needs_same_entry_within_interval() {
        let t = Instant::now();
        assert!(!is_double_click(None, t, 3));
        assert!(is_double_click(
            Some((t, 3)),
            t + Duration::from_millis(300),
            3
        ));
        assert!(!is_double_click(
            Some((t, 2)),
            t + Duration::from_millis(300),
            3
        ));
        assert!(!is_double_click(
            Some((t, 3)),
            t + Duration::from_millis(900),
            3
        ));
    }

    #[test]
    fn natural_order_compares_numbers_and_ignores_case() {
        let mut v = vec!["file10", "File2", "file1", "b", "A"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["A", "b", "file1", "File2", "file10"]);
    }

    #[test]
    fn list_dir_puts_dirs_first_and_hides_dotfiles() {
        let dir = std::env::temp_dir().join(format!("folio-browse-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("b.txt"), "b").unwrap();
        fs::write(dir.join("a10.txt"), "a").unwrap();
        fs::write(dir.join("a2.txt"), "a").unwrap();
        fs::write(dir.join(".hidden"), "h").unwrap();
        let names = |show: bool| -> Vec<String> {
            list_dir(&dir, show)
                .unwrap()
                .into_iter()
                .map(|e| e.name)
                .collect()
        };
        assert_eq!(names(false), vec!["sub", "a2.txt", "a10.txt", "b.txt"]);
        assert_eq!(names(true)[1], ".hidden");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn preview_skips_binary_and_reports_size() {
        let dir = std::env::temp_dir().join(format!("folio-preview-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("x.bin");
        fs::write(&bin, [0xff, 0xfe, 0x00]).unwrap();
        assert!(matches!(read_for_preview(&bin), PreviewSource::Skip(r) if r.contains("UTF-8")));
        let txt = dir.join("x.md");
        fs::write(&txt, "# hi").unwrap();
        assert_eq!(read_for_preview(&txt), PreviewSource::Text("# hi".into()));
        assert!(matches!(
            read_for_preview(&dir.join("missing")),
            PreviewSource::Skip(_)
        ));
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1536), "1.5KB");
        assert_eq!(human_size(PREVIEW_MAX_BYTES), "1.0MB");
    }
}
