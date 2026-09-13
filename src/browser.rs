//! `folio browse`。左にディレクトリ一覧、右にプレビューの2ペインのファイラー（第一歩）。
//! ファイル操作はしない。ターミナル制御はこのモジュールに閉じ込め、プレビューの描画は
//! ビューアーと同じ中核（format / render）を使う。

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use unicode_width::UnicodeWidthStr;

use crate::config::Config;
use crate::fileops;
use crate::format::Format;
use crate::highlight::Highlighter;
use crate::render::{self, Options, Theme};
use crate::viewer;

/// プレビューで読む上限（バイト）。これより大きいファイルは読まない。
pub const PREVIEW_MAX_BYTES: u64 = 1024 * 1024;

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
    let result = Browser::new(start, config).and_then(|mut b| b.run(&mut terminal));
    ratatui::restore();
    result
}

/// 右ペインの状態。カーソルの項目が変わった時だけ作り直す。
struct Preview {
    path: PathBuf,
    width: u16,
    lines: Vec<Line<'static>>,
    scroll: usize,
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
}

/// 下部の1行で受ける入力と確認。
enum Prompt {
    /// 新規作成の名前
    Create { input: String },
    /// 改名。`path`が対象
    Rename { path: PathBuf, input: String },
    /// ごみ箱へ移す確認。`y`だけが実行
    Trash { path: PathBuf, name: String },
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
    }

    fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<PathBuf> {
        while !self.quit {
            terminal.draw(|frame| self.draw(frame))?;
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.key(terminal, key)?;
            }
        }
        Ok(self.cwd.clone())
    }

    fn key(&mut self, terminal: &mut DefaultTerminal, key: KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.prompt.is_some() {
            self.prompt_key(key);
            return Ok(());
        }
        self.message = None;
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
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
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
            Prompt::Trash { .. } => {
                let confirmed = matches!(key.code, KeyCode::Char('y' | 'Y'));
                let Some(Prompt::Trash { path, name }) = self.prompt.take() else {
                    return;
                };
                if confirmed {
                    self.trash(&path, &name);
                } else {
                    self.message = Some("中止しました".to_string());
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
                        Prompt::Trash { .. } => {}
                    }
                }
                _ => {}
            },
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
        if let Some(p) = &mut self.preview {
            p.scroll = p.scroll.saturating_add_signed(delta);
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
        self.draw_list(frame, list);
        self.draw_preview(frame, preview);
        self.draw_status(frame, status);
    }

    fn draw_list(&self, frame: &mut Frame, area: Rect) {
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
        let list = List::new(items)
            .block(Block::new().borders(Borders::RIGHT).title(title))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▸");
        let mut state = ListState::default();
        if !self.entries.is_empty() {
            state.select(Some(self.cursor));
        }
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_preview(&mut self, frame: &mut Frame, area: Rect) {
        let inner = Rect {
            x: area.x + 1,
            width: area.width.saturating_sub(1),
            ..area
        };
        self.ensure_preview(inner.width);
        let Some(p) = &self.preview else {
            return;
        };
        let height = inner.height as usize;
        let max_scroll = p.lines.len().saturating_sub(height);
        let scroll = p.scroll.min(max_scroll);
        let end = (scroll + height).min(p.lines.len());
        let text = Text::from(p.lines[scroll..end].to_vec());
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
        let lines = self.build_preview(&entry, width);
        self.preview = Some(Preview {
            path: entry.path,
            width,
            lines,
            scroll,
        });
    }

    fn build_preview(&self, entry: &Entry, width: u16) -> Vec<Line<'static>> {
        if entry.is_dir {
            return match list_dir(&entry.path, self.show_hidden) {
                Ok(entries) if entries.is_empty() => vec![Line::from("（空のディレクトリ）").dim()],
                Ok(entries) => entries
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
                    .collect(),
                Err(err) => vec![Line::from(err.to_string()).red()],
            };
        }
        match read_for_preview(&entry.path) {
            PreviewSource::Skip(reason) => vec![Line::from(reason).dim()],
            PreviewSource::Text(source) => {
                let format = Format::detect(&entry.path, |e| self.highlighter.supports(e));
                let blocks = format.parse(&entry.path, &source);
                render::render_with(
                    &blocks,
                    width.saturating_sub(2),
                    &self.theme,
                    Some(&self.highlighter),
                    &Options::default(),
                )
                .lines
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
            (None, Some(m)) => format!(" {m}"),
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
            Some(Prompt::Trash { .. }) => "y:実行  それ以外:中止 ",
            Some(_) => "Enter:確定  Esc:中止 ",
            None => "h/l:親/開く  Enter:読む  e:編集  a:作成  r:改名  d:ごみ箱  .:隠し  q:終了 ",
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
