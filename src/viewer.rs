//! ターミナルに文書を表示し、キー操作でスクロールするビューアー。
//! ターミナル制御はこのモジュールに閉じ込める。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use unicode_width::UnicodeWidthStr;

use crate::config::Config;
use crate::document;
use crate::format::Format;
use crate::highlight::Highlighter;
use crate::render::{self, Options, Rendered, Theme};
use crate::search::{self, Match};
use crate::select::{self, Dragger, Selection};

/// 折り返し幅の右に空けておく桁数（ぶら下げ句読点の逃げ場）。
const HANG_RESERVE: u16 = 2;

/// 目次ペインの幅。画面の1/4か、この最小幅の広い方。
const TOC_MIN_WIDTH: u16 = 24;

/// ホイール1回で動かす行数（項目数）。
pub const WHEEL_STEP: isize = 5;

/// 端末にマウスを受け取らせているか。エディタに渡す間だけ止めて戻すために覚えておく。
static MOUSE_CAPTURE: AtomicBool = AtomicBool::new(false);

/// マウスの受け取りを入れる／切る。受け取る間は、端末の文字選択が`Shift`+ドラッグに変わる。
pub fn set_mouse_capture(on: bool) -> Result<()> {
    let mut out = std::io::stdout();
    if on {
        execute!(out, EnableMouseCapture)?;
    } else {
        execute!(out, DisableMouseCapture)?;
    }
    MOUSE_CAPTURE.store(on, Ordering::Relaxed);
    Ok(())
}

/// `folio render <path>`の本体。TUIを開かず、ANSI付きの行を標準出力に流す。
/// yaziのプレビュー欄（piper経由）や`less -R`から使う。
pub fn render_to_stdout(
    path: &Path,
    width: u16,
    config: &Config,
    format: Option<Format>,
) -> Result<()> {
    use std::io::Write;
    let source = read(path)?;
    let theme = config.theme()?;
    let highlighter = Highlighter::with_theme(&config.highlight.theme);
    let format = format.unwrap_or_else(|| Format::detect(path, |e| highlighter.supports(e)));
    let blocks = format.parse(path, &source);
    let rendered = render::render_with(
        &blocks,
        width,
        &theme,
        Some(&highlighter),
        &Options::default(),
    );
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for line in &rendered.lines {
        // 受け側が閉じたら（プレビューの打ち切りなど）静かに終える
        if writeln!(out, "{}", crate::ansi::line_to_ansi(line)).is_err() {
            break;
        }
    }
    let _ = out.flush();
    Ok(())
}

/// `folio view <path>`の本体。ファイルを読み、閉じるまでターミナルを占有する。
pub fn run(path: &Path, config: &Config, format: Option<Format>) -> Result<()> {
    let mut terminal = ratatui::init();
    let result =
        set_mouse_capture(true).and_then(|()| view_in(&mut terminal, path, config, format));
    let _ = set_mouse_capture(false);
    ratatui::restore();
    result
}

/// 既に開いているターミナルの上でビューアーを動かし、`q`で戻る（ファイラーから使う）。
/// 戻る時に画面を消す。
pub fn view_in(
    terminal: &mut DefaultTerminal,
    path: &Path,
    config: &Config,
    format: Option<Format>,
) -> Result<()> {
    let source = read(path)?;
    let mut app = App::new(path, &source, config, format)?;
    let result = app.run(terminal);
    terminal.clear()?;
    result
}

/// TUIを抜けて`$VISUAL`/`$EDITOR`（既定はvim）で`path`を開き、閉じたらTUIに戻る。
/// 外側の`Err`はターミナルの再初期化の失敗、内側の`Err`はエディタ側の失敗（表示用の文）。
pub fn open_editor(terminal: &mut DefaultTerminal, path: &Path) -> Result<Result<(), String>> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vim".to_string());
    // 受け取ったままだと、クリックが文字列としてエディタに届く
    let mouse = MOUSE_CAPTURE.load(Ordering::Relaxed);
    if mouse {
        set_mouse_capture(false)?;
    }
    ratatui::restore();
    // $EDITORは引数付きのこともある（`code -w`など）ので、シェルに展開させる
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("folio")
        .arg(path)
        .status();
    *terminal = ratatui::init();
    if mouse {
        set_mouse_capture(true)?;
    }
    terminal.clear()?;
    Ok(match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("エディタが異常終了しました（{s}）")),
        Err(err) => Err(format!("エディタを起動できません（{editor}）: {err}")),
    })
}

/// ドラッグで選び終えた時の知らせ。
pub const SELECTED_HINT: &str = "選択しました（Ctrl-cでコピー、Escで解除）";

/// 選択中のキー。`Ctrl-c`で`text`をクリップボードへ送り（選択は残す）、`Esc`で解除する。
/// 扱ったら`true`。選択が無ければ何もしない（`Ctrl-c`は呼び出し側の終了に回る）。
pub fn selection_key(
    drag: &mut Dragger,
    key: KeyEvent,
    message: &mut Option<String>,
    text: impl FnOnce(Selection) -> String,
) -> Result<bool> {
    let Some(sel) = drag.selection() else {
        return Ok(false);
    };
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let text = text(sel);
            copy_to_clipboard(&text)?;
            *message = Some(format!("コピーしました（{}文字）", text.chars().count()));
            Ok(true)
        }
        KeyCode::Esc => {
            drag.clear();
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// 端末経由でクリップボードへ送る（OSC 52）。
pub fn copy_to_clipboard(text: &str) -> Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout();
    out.write_all(select::osc52(text).as_bytes())?;
    out.flush()?;
    Ok(())
}

/// ファイルをUTF-8として読む。文字コードの自動判定はしない。
pub fn read(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("{}を読めません", path.display()))?;
    String::from_utf8(bytes).with_context(|| {
        format!(
            "{}はUTF-8ではありません（他の文字コードには未対応）",
            path.display()
        )
    })
}

/// キー操作の結果のうち、ターミナルを触る必要があるもの。
enum Action {
    None,
    Edit,
}

struct App {
    path: PathBuf,
    format: Format,
    blocks: Vec<document::Block>,
    rendered: Rendered,
    rendered_width: u16,
    theme: Theme,
    highlighter: Highlighter,
    scroll: usize,
    page_height: usize,
    quit: bool,
    /// ステータス行に出す一時的な知らせ（次のキーで消える）
    message: Option<String>,
    /// 目次ペインを開いているか
    toc_open: bool,
    /// 目次内のカーソル位置（見出しの添字）
    toc_cursor: usize,
    /// `]]` `[[`のような2打鍵の1打目
    pending_key: Option<char>,
    /// 画面全体の幅（ステータス行の右寄せに使う）
    screen_width: u16,
    /// 描き方の切り替え（URL表示など）
    options: Options,
    /// 本文の折り返しの最大幅
    max_width: u16,
    /// 本文の左右の余白
    margin: u16,
    /// `/`で入力中の文字列（`None`なら入力モードでない）
    input: Option<String>,
    /// 確定した検索
    search: Option<Search>,
    /// 直前の描画での本文の領域（マウスの位置を行に直す）
    body: Rect,
    /// ドラッグでの文字の選択
    drag: Dragger,
}

struct Search {
    query: String,
    matches: Vec<Match>,
    /// 今いるヒットの添字
    current: usize,
}

impl App {
    fn new(path: &Path, source: &str, config: &Config, format: Option<Format>) -> Result<Self> {
        let highlighter = Highlighter::with_theme(&config.highlight.theme);
        let format = format.unwrap_or_else(|| Format::detect(path, |e| highlighter.supports(e)));
        Ok(Self {
            path: path.to_path_buf(),
            format,
            blocks: format.parse(path, source),
            rendered: Rendered::default(),
            rendered_width: 0,
            theme: config.theme()?,
            highlighter,
            scroll: 0,
            page_height: 1,
            quit: false,
            message: None,
            toc_open: false,
            toc_cursor: 0,
            pending_key: None,
            screen_width: 0,
            options: Options::default(),
            max_width: config.view.max_width,
            margin: config.view.margin,
            input: None,
            search: None,
            body: Rect::default(),
            drag: Dragger::default(),
        })
    }

    /// ファイルを読み直す。失敗したら前の内容を残し、ステータス行で知らせる。
    fn reload(&mut self) {
        match read(&self.path) {
            Ok(source) => {
                self.blocks = self.format.parse(&self.path, &source);
                self.rendered_width = 0;
            }
            Err(err) => self.message = Some(format!("再読み込みに失敗: {err}")),
        }
    }

    /// TUIを抜けて`$EDITOR`（既定はvim）で開き、閉じたらTUIに戻って読み直す。
    fn edit(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        match open_editor(terminal, &self.path)? {
            Ok(()) => self.reload(),
            Err(message) => self.message = Some(message),
        }
        Ok(())
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut dirty = true;
        while !self.quit {
            if dirty {
                terminal.draw(|frame| self.draw(frame))?;
            }
            dirty = true;
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    self.message = None;
                    let lines = &self.rendered;
                    if !selection_key(&mut self.drag, key, &mut self.message, |s| {
                        select::text(&lines.lines, &lines.flow, &lines.tables, s)
                    })? {
                        self.drag.clear();
                        match self.key(key) {
                            Action::None => {}
                            Action::Edit => self.edit(terminal)?,
                        }
                    }
                }
                Event::Mouse(m) => dirty = self.mouse(m)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// マウス。ホイールで本文を動かし、ドラッグで文字を選ぶ（コピーは`Ctrl-c`）。
    /// 画面が変わる時に`true`。
    fn mouse(&mut self, m: MouseEvent) -> Result<bool> {
        let total = self.rendered.lines.len();
        match m.kind {
            MouseEventKind::ScrollDown => self.scroll_by(WHEEL_STEP),
            MouseEventKind::ScrollUp => self.scroll_by(-WHEEL_STEP),
            MouseEventKind::Down(MouseButton::Left) => {
                self.message = None;
                let point = select::point_at(self.body, self.scroll, total, m.column, m.row);
                match point {
                    Some(p) if self.body.contains(Position::new(m.column, m.row)) => {
                        // 表のセルの中で始めたドラッグは、そのセルから出さない
                        let cell = render::cell_at(&self.rendered.tables, p.line, p.col);
                        self.drag.begin(p, cell)
                    }
                    _ => self.drag.clear(),
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.drag.is_dragging() => {
                // 本文の上下にはみ出したら、その向きに1行ずつ送る
                self.scroll_by(select::edge_scroll(self.body, m.row));
                self.scroll = self.scroll.min(total.saturating_sub(self.page_height));
                if let Some(p) = select::point_at(self.body, self.scroll, total, m.column, m.row) {
                    self.drag.extend(p);
                }
            }
            MouseEventKind::Up(MouseButton::Left) if self.drag.is_dragging() => {
                if self.drag.finish().is_some() {
                    self.message = Some(SELECTED_HINT.to_string());
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// 本文の折り返し幅が変わっていたら描き直す。
    fn ensure_rendered(&mut self, wrap_width: u16) {
        if wrap_width == self.rendered_width {
            return;
        }
        // 幅が変わると行番号がずれるので、直前の見出しからの相対位置で画面上端を保つ
        let anchor = self
            .current_heading()
            .map(|i| (i, self.scroll - self.rendered.headings[i].line));
        self.rendered = render::render_with(
            &self.blocks,
            wrap_width,
            &self.theme,
            Some(&self.highlighter),
            &self.options,
        );
        self.rendered_width = wrap_width;
        // 行の位置が変わるので、選択は捨てる
        self.drag.clear();
        if let Some((i, offset)) = anchor
            && let Some(h) = self.rendered.headings.get(i)
        {
            self.scroll = h.line + offset;
        }
        if let Some(s) = &mut self.search {
            s.matches = search::find_all(&self.rendered.lines, &s.query);
            s.current = s.current.min(s.matches.len().saturating_sub(1));
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [main, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        self.screen_width = frame.area().width;
        let (toc, body) = if self.toc_open {
            let width = (main.width / 4).max(TOC_MIN_WIDTH).min(main.width / 2);
            let [toc, body] =
                Layout::horizontal([Constraint::Length(width), Constraint::Fill(1)]).areas(main);
            (Some(toc), body)
        } else {
            (None, main)
        };
        let body = body.inner(Margin::new(self.margin, 0));
        // 広い端末では本文を最大幅の列に収め、余りは左右に均等に配る（リーダー表示）
        let body = centered_column(body, self.max_width + HANG_RESERVE);
        self.body = body;

        // 行頭禁則でぶら下がる句読点（最大2桁）を切らないよう、折り返し幅は描画領域より2桁狭くする
        self.ensure_rendered(body.width.saturating_sub(HANG_RESERVE));
        self.page_height = body.height.max(1) as usize;
        let total = self.rendered.lines.len();
        let max_scroll = total.saturating_sub(self.page_height);
        self.scroll = self.scroll.min(max_scroll);
        let end = (self.scroll + self.page_height).min(total);

        let visible: Vec<Line<'static>> = (self.scroll..end)
            .map(|i| self.line_with_matches(i))
            .collect();
        frame.render_widget(Paragraph::new(Text::from(visible)), body);
        if let Some(toc) = toc {
            self.draw_toc(frame, toc);
        }
        frame.render_widget(self.status_line(end, total), status);
    }

    /// 行にヒットの強調と選択を重ねる。今いるヒットは黄色、他は暗い黄色の背景。
    fn line_with_matches(&self, i: usize) -> Line<'static> {
        let line = &self.rendered.lines[i];
        let mut ranges: Vec<(usize, usize, Style)> = Vec::new();
        if let Some(s) = &self.search {
            ranges.extend(
                s.matches
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.line == i)
                    .map(|(k, m)| {
                        let style = if k == s.current {
                            Style::new()
                                .bg(Color::Yellow)
                                .fg(Color::Black)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::new().bg(Color::Indexed(58)).fg(Color::White)
                        };
                        (m.start, m.end, style)
                    }),
            );
        }
        if let Some(sel) = self.drag.selection()
            && let Some((from, to)) = select::chars_in_line(line, &self.rendered.flow, sel, i)
        {
            ranges.push((from, to, select::style()));
        }
        search::highlight(line, &ranges)
    }

    fn draw_toc(&mut self, frame: &mut Frame, area: Rect) {
        let current = self.current_heading();
        let items: Vec<ListItem> = if self.rendered.headings.is_empty() {
            vec![ListItem::new("（見出しなし）").dim()]
        } else {
            self.rendered
                .headings
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let indent = " ".repeat((h.level.saturating_sub(1)) as usize * 2);
                    let mut style = Style::default();
                    if Some(i) == current {
                        style = style.fg(Color::Yellow).add_modifier(Modifier::BOLD);
                    }
                    ListItem::new(Line::from(vec![
                        Span::raw(indent),
                        Span::styled(h.text.clone(), style),
                    ]))
                })
                .collect()
        };
        let list = List::new(items)
            .block(Block::new().borders(Borders::RIGHT).title(" 目次 "))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▸");
        let mut state = ListState::default();
        if !self.rendered.headings.is_empty() {
            self.toc_cursor = self.toc_cursor.min(self.rendered.headings.len() - 1);
            state.select(Some(self.toc_cursor));
        }
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn status_line(&self, end: usize, total: usize) -> Paragraph<'static> {
        let percent = (end * 100).checked_div(total).unwrap_or(100);
        let name = self.path.display().to_string();
        let left = match (&self.input, &self.message, &self.search) {
            (Some(input), _, _) => format!(" /{input}_"),
            (_, Some(message), _) => format!(" {message}"),
            (_, _, Some(s)) if !s.matches.is_empty() => format!(
                " {name}  {}-{end}/{total} ({percent}%)  /{}  {}/{}件",
                self.scroll + 1,
                s.query,
                s.current + 1,
                s.matches.len()
            ),
            _ => format!(" {name}  {}-{end}/{total} ({percent}%)", self.scroll + 1),
        };
        let right = if self.input.is_some() {
            "Enter:検索  Esc:やめる ".to_string()
        } else if self.toc_open {
            "j/k:選ぶ  Enter:飛ぶ  t/Esc:閉じる  q:終了 ".to_string()
        } else if self.search.is_some() {
            "n/N:次/前のヒット  Esc:検索解除  /:再検索  q:終了 ".to_string()
        } else {
            "j/k d/u gg/G:移動  ]]/[[:見出し  t:目次  /:検索  U:URL  e:編集  r:再読込  q:終了 "
                .to_string()
        };
        let gap = (self.screen_width as usize).saturating_sub(left.width() + right.width());
        let line = Line::from(vec![
            Span::raw(left),
            Span::raw(" ".repeat(gap)),
            Span::raw(right).dim(),
        ]);
        Paragraph::new(line).style(Style::new().bg(Color::Indexed(238)).fg(Color::White))
    }

    /// 画面上端が属する見出し（その行以前で最後の見出し）。
    fn current_heading(&self) -> Option<usize> {
        current_heading(&self.rendered.headings, self.scroll)
    }

    fn key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(input) = &mut self.input {
            match key.code {
                KeyCode::Char('c') if ctrl => self.quit = true,
                KeyCode::Esc => self.input = None,
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Enter => {
                    let query = self.input.take().unwrap_or_default();
                    self.start_search(query);
                }
                KeyCode::Char(ch) => input.push(ch),
                _ => {}
            }
            return Action::None;
        }
        // 2打鍵（]] / [[）の処理
        if let Some(first) = self.pending_key.take() {
            match (first, key.code) {
                (']', KeyCode::Char(']')) => {
                    self.jump_heading(1);
                    return Action::None;
                }
                ('[', KeyCode::Char('[')) => {
                    self.jump_heading(-1);
                    return Action::None;
                }
                ('g', KeyCode::Char('g')) => {
                    if self.toc_open {
                        self.toc_cursor = 0;
                    } else {
                        self.scroll = 0;
                    }
                    return Action::None;
                }
                _ => {} // 組にならなければ2打目を通常のキーとして扱う
            }
        }
        if self.toc_open {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.toc_move(1),
                KeyCode::Char('k') | KeyCode::Up => self.toc_move(-1),
                KeyCode::Char('g') => self.pending_key = Some('g'),
                KeyCode::Home => self.toc_cursor = 0,
                KeyCode::Char('G') | KeyCode::End => self.toc_cursor = usize::MAX,
                KeyCode::Enter => {
                    if let Some(h) = self.rendered.headings.get(self.toc_cursor) {
                        self.scroll = h.line;
                    }
                }
                KeyCode::Char('t') | KeyCode::Esc => self.toc_open = false,
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Char('c') if ctrl => self.quit = true,
                _ => {}
            }
            return Action::None;
        }
        let half = (self.page_height / 2).max(1);
        match key.code {
            KeyCode::Char(']') => self.pending_key = Some(']'),
            KeyCode::Char('[') => self.pending_key = Some('['),
            KeyCode::Char('t') => {
                self.toc_open = true;
                self.toc_cursor = self.current_heading().unwrap_or(0);
            }
            KeyCode::Char('U') => {
                self.options.show_urls = !self.options.show_urls;
                self.rendered_width = 0; // 描き直す（位置は見出し基準で保たれる）
            }
            KeyCode::Char('/') => self.input = Some(String::new()),
            KeyCode::Char('n') => self.next_match(1),
            KeyCode::Char('N') => self.next_match(-1),
            KeyCode::Char('e') => return Action::Edit,
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('q') => self.quit = true,
            // Escは「解除」に使い、何も解除するものが無い時だけ終了
            KeyCode::Esc if self.search.is_some() => self.search = None,
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('d') => self.scroll_by(half as isize),
            KeyCode::Char('u') => self.scroll_by(-(half as isize)),
            KeyCode::Char('f') if ctrl => self.scroll_by(self.page_height as isize),
            KeyCode::Char('b') if ctrl => self.scroll_by(-(self.page_height as isize)),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll_by(self.page_height as isize),
            KeyCode::PageUp => self.scroll_by(-(self.page_height as isize)),
            KeyCode::Char('g') => self.pending_key = Some('g'),
            KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = usize::MAX,
            _ => {}
        }
        Action::None
    }

    /// 検索を確定し、画面上端以降で最初のヒットへ移る。
    fn start_search(&mut self, query: String) {
        if query.is_empty() {
            self.search = None;
            return;
        }
        let matches = search::find_all(&self.rendered.lines, &query);
        if matches.is_empty() {
            self.message = Some(format!("見つかりません: {query}"));
            self.search = None;
            return;
        }
        let current = matches
            .iter()
            .position(|m| m.line >= self.scroll)
            .unwrap_or(0);
        self.search = Some(Search {
            query,
            matches,
            current,
        });
        self.scroll_to_current_match();
    }

    /// 次（+1）／前（-1）のヒットへ。端まで行ったら反対側へ回る。
    fn next_match(&mut self, direction: isize) {
        let Some(s) = &mut self.search else { return };
        let len = s.matches.len();
        if len == 0 {
            return;
        }
        s.current = (s.current as isize + direction).rem_euclid(len as isize) as usize;
        self.scroll_to_current_match();
    }

    /// 今いるヒットが画面に無ければ、画面の上から1/3の位置に来るよう動かす。
    fn scroll_to_current_match(&mut self) {
        let Some(s) = &self.search else { return };
        let Some(m) = s.matches.get(s.current) else {
            return;
        };
        let visible = self.scroll..self.scroll + self.page_height;
        if !visible.contains(&m.line) {
            self.scroll = m.line.saturating_sub(self.page_height / 3);
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
        // 上限はdraw側で行数に合わせて詰める
    }

    fn toc_move(&mut self, delta: isize) {
        let len = self.rendered.headings.len();
        if len == 0 {
            return;
        }
        self.toc_cursor = self.toc_cursor.saturating_add_signed(delta).min(len - 1);
    }

    /// 次（+1）／前（-1）の見出しへ画面上端を移す。
    fn jump_heading(&mut self, direction: isize) {
        let target = if direction > 0 {
            next_heading(&self.rendered.headings, self.scroll)
        } else {
            prev_heading(&self.rendered.headings, self.scroll)
        };
        if let Some(i) = target {
            self.scroll = self.rendered.headings[i].line;
            self.toc_cursor = i;
        }
    }
}

/// `area`の中に幅`max`までの列を中央に切り出す。狭ければ`area`のまま。
fn centered_column(area: Rect, max: u16) -> Rect {
    if area.width <= max {
        return area;
    }
    let offset = (area.width - max) / 2;
    Rect {
        x: area.x + offset,
        width: max,
        ..area
    }
}

fn current_heading(headings: &[render::Heading], scroll: usize) -> Option<usize> {
    headings.iter().rposition(|h| h.line <= scroll)
}

fn next_heading(headings: &[render::Heading], scroll: usize) -> Option<usize> {
    headings.iter().position(|h| h.line > scroll)
}

fn prev_heading(headings: &[render::Heading], scroll: usize) -> Option<usize> {
    headings.iter().rposition(|h| h.line < scroll)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEvent;

    fn app(source: &str) -> App {
        let mut app = App::new(Path::new("test.md"), source, &Config::default(), None).unwrap();
        app.ensure_rendered(40);
        app.page_height = 10;
        app
    }

    fn press(app: &mut App, ch: char) {
        app.key(KeyEvent::from(KeyCode::Char(ch)));
    }

    const DOC: &str = "# A\n\none\n\n## B\n\ntwo\n\n# C\n\nthree\n";

    #[test]
    fn heading_positions() {
        let app = app(DOC);
        let lines: Vec<usize> = app.rendered.headings.iter().map(|h| h.line).collect();
        // 見出しの下の罫線1行があるので: A(0) ━ 空 one 空 B(5) ─ 空 two 空 C(10)
        assert_eq!(lines, vec![0, 5, 10]);
        assert_eq!(current_heading(&app.rendered.headings, 6), Some(1));
        assert_eq!(next_heading(&app.rendered.headings, 6), Some(2));
        // 見出しの行にいる時の「前」はその1つ前（vimの[[と同じ）
        assert_eq!(prev_heading(&app.rendered.headings, 5), Some(0));
        assert_eq!(prev_heading(&app.rendered.headings, 6), Some(1));
        assert_eq!(prev_heading(&app.rendered.headings, 0), None);
    }

    #[test]
    fn double_bracket_jumps_between_headings() {
        let mut app = app(DOC);
        press(&mut app, ']');
        press(&mut app, ']');
        assert_eq!(app.scroll, 5);
        press(&mut app, ']');
        press(&mut app, ']');
        assert_eq!(app.scroll, 10);
        press(&mut app, '[');
        press(&mut app, '[');
        assert_eq!(app.scroll, 5);
    }

    #[test]
    fn rerender_keeps_position_relative_to_heading() {
        let source = format!(
            "# A\n\n{}\n\n# B\n\n{}\n",
            "word ".repeat(60),
            "word ".repeat(60)
        );
        let mut app = App::new(Path::new("t.md"), &source, &Config::default(), None).unwrap();
        app.ensure_rendered(40);
        app.scroll = app.rendered.headings[1].line + 2;
        app.ensure_rendered(20);
        assert_eq!(app.scroll, app.rendered.headings[1].line + 2);
    }

    fn type_str(app: &mut App, s: &str) {
        for ch in s.chars() {
            press(app, ch);
        }
    }

    #[test]
    fn slash_searches_and_n_cycles_with_wraparound() {
        let mut app = app("x\n\nfoo\n\ny\n\nfoo bar\n\nfoo\n");
        press(&mut app, '/');
        type_str(&mut app, "foo");
        app.key(KeyEvent::from(KeyCode::Enter));
        let s = app.search.as_ref().unwrap();
        assert_eq!(s.matches.len(), 3);
        assert_eq!(s.current, 0);
        press(&mut app, 'n');
        assert_eq!(app.search.as_ref().unwrap().current, 1);
        press(&mut app, 'n');
        press(&mut app, 'n');
        assert_eq!(app.search.as_ref().unwrap().current, 0);
        press(&mut app, 'N');
        assert_eq!(app.search.as_ref().unwrap().current, 2);
        app.key(KeyEvent::from(KeyCode::Esc));
        assert!(app.search.is_none());
        assert!(!app.quit);
    }

    #[test]
    fn search_moves_view_to_offscreen_hit() {
        let body: String = (0..40).map(|i| format!("line {i}\n\n")).collect();
        let mut app = app(&body);
        press(&mut app, '/');
        type_str(&mut app, "line 30");
        app.key(KeyEvent::from(KeyCode::Enter));
        let m = app.search.as_ref().unwrap().matches[0];
        assert!(app.scroll <= m.line && m.line < app.scroll + app.page_height);
    }

    #[test]
    fn no_hit_shows_message_and_esc_cancels_input() {
        let mut app = app("abc\n");
        press(&mut app, '/');
        type_str(&mut app, "zzz");
        app.key(KeyEvent::from(KeyCode::Enter));
        assert!(app.search.is_none());
        assert!(app.message.as_deref().unwrap().contains("zzz"));
        press(&mut app, '/');
        type_str(&mut app, "ab");
        app.key(KeyEvent::from(KeyCode::Esc));
        assert!(app.input.is_none() && app.search.is_none() && !app.quit);
    }

    #[test]
    fn centered_column_only_shrinks_wide_areas() {
        let area = Rect::new(2, 0, 50, 10);
        assert_eq!(centered_column(area, 60), area);
        let c = centered_column(area, 30);
        assert_eq!((c.x, c.width, c.height), (12, 30, 10));
    }

    #[test]
    fn shift_u_toggles_url_display() {
        let mut app = app("[a](http://x)\n");
        assert_eq!(search::plain(&app.rendered.lines[0]), "a");
        press(&mut app, 'U');
        app.ensure_rendered(40);
        assert_eq!(search::plain(&app.rendered.lines[0]), "a (http://x)");
        press(&mut app, 'U');
        app.ensure_rendered(40);
        assert_eq!(search::plain(&app.rendered.lines[0]), "a");
    }

    #[test]
    fn gg_goes_to_top_and_single_g_does_not() {
        let mut app = app(DOC);
        app.scroll = 7;
        press(&mut app, 'g');
        assert_eq!(app.scroll, 7);
        press(&mut app, 'g');
        assert_eq!(app.scroll, 0);
        // 目次でも同じ
        app.scroll = 7;
        press(&mut app, 't');
        assert_eq!(app.toc_cursor, 1);
        press(&mut app, 'g');
        press(&mut app, 'g');
        assert_eq!(app.toc_cursor, 0);
    }

    #[test]
    fn unpaired_bracket_falls_through_to_normal_key() {
        let mut app = app(DOC);
        press(&mut app, ']');
        press(&mut app, 'j');
        assert_eq!(app.scroll, 1);
        assert!(app.pending_key.is_none());
    }

    #[test]
    fn toc_opens_at_current_heading_and_enter_jumps() {
        let mut app = app(DOC);
        app.scroll = 6;
        press(&mut app, 't');
        assert!(app.toc_open);
        assert_eq!(app.toc_cursor, 1);
        press(&mut app, 'j');
        app.key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.scroll, 10);
        press(&mut app, 't');
        assert!(!app.toc_open);
    }
}
