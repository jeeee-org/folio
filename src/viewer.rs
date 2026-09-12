//! ターミナルに文書を表示し、キー操作でスクロールするビューアー。
//! ターミナル制御はこのモジュールに閉じ込める。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use unicode_width::UnicodeWidthStr;

use crate::document;
use crate::highlight::Highlighter;
use crate::render::{self, Rendered, Theme};

/// 折り返し幅の右に空けておく桁数（ぶら下げ句読点の逃げ場）。
const HANG_RESERVE: u16 = 2;

/// 目次ペインの幅。画面の1/4か、この最小幅の広い方。
const TOC_MIN_WIDTH: u16 = 24;

/// `folio render <path>`の本体。TUIを開かず、ANSI付きの行を標準出力に流す。
/// yaziのプレビュー欄（piper経由）や`less -R`から使う。
pub fn render_to_stdout(path: &Path, width: u16) -> Result<()> {
    use std::io::Write;
    let source =
        fs::read_to_string(path).with_context(|| format!("{}を読めません", path.display()))?;
    let blocks = document::parse(&source);
    let rendered =
        render::render_with(&blocks, width, &Theme::default(), Some(&Highlighter::new()));
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
pub fn run(path: &Path) -> Result<()> {
    let source =
        fs::read_to_string(path).with_context(|| format!("{}を読めません", path.display()))?;
    let mut app = App::new(path, &source);
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

/// キー操作の結果のうち、ターミナルを触る必要があるもの。
enum Action {
    None,
    Edit,
}

struct App {
    path: PathBuf,
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
}

impl App {
    fn new(path: &Path, source: &str) -> Self {
        Self {
            path: path.to_path_buf(),
            blocks: document::parse(source),
            rendered: Rendered::default(),
            rendered_width: 0,
            theme: Theme::default(),
            highlighter: Highlighter::new(),
            scroll: 0,
            page_height: 1,
            quit: false,
            message: None,
            toc_open: false,
            toc_cursor: 0,
            pending_key: None,
            screen_width: 0,
        }
    }

    /// ファイルを読み直す。失敗したら前の内容を残し、ステータス行で知らせる。
    fn reload(&mut self) {
        match fs::read_to_string(&self.path) {
            Ok(source) => {
                self.blocks = document::parse(&source);
                self.rendered_width = 0;
            }
            Err(err) => self.message = Some(format!("再読み込みに失敗: {err}")),
        }
    }

    /// TUIを抜けて`$EDITOR`（既定はvim）で開き、閉じたらTUIに戻って読み直す。
    fn edit(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "vim".to_string());
        ratatui::restore();
        // $EDITORは引数付きのこともある（`code -w`など）ので、シェルに展開させる
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("folio")
            .arg(&self.path)
            .status();
        *terminal = ratatui::init();
        terminal.clear()?;
        match status {
            Ok(s) if s.success() => self.reload(),
            Ok(s) => self.message = Some(format!("エディタが異常終了しました（{s}）")),
            Err(err) => self.message = Some(format!("エディタを起動できません（{editor}）: {err}")),
        }
        Ok(())
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            terminal.draw(|frame| self.draw(frame))?;
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.message = None;
                match self.key(key) {
                    Action::None => {}
                    Action::Edit => self.edit(terminal)?,
                }
            }
        }
        Ok(())
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
        );
        self.rendered_width = wrap_width;
        if let Some((i, offset)) = anchor
            && let Some(h) = self.rendered.headings.get(i)
        {
            self.scroll = h.line + offset;
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
        let body = body.inner(Margin::new(1, 0));

        // 行頭禁則でぶら下がる句読点（最大2桁）を切らないよう、折り返し幅は描画領域より2桁狭くする
        self.ensure_rendered(body.width.saturating_sub(HANG_RESERVE));
        self.page_height = body.height.max(1) as usize;
        let total = self.rendered.lines.len();
        let max_scroll = total.saturating_sub(self.page_height);
        self.scroll = self.scroll.min(max_scroll);
        let end = (self.scroll + self.page_height).min(total);

        let text = Text::from(self.rendered.lines[self.scroll..end].to_vec());
        frame.render_widget(Paragraph::new(text), body);
        if let Some(toc) = toc {
            self.draw_toc(frame, toc);
        }
        frame.render_widget(self.status_line(end, total), status);
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
        let left = match &self.message {
            Some(message) => format!(" {message}"),
            None => format!(" {name}  {}-{end}/{total} ({percent}%)", self.scroll + 1),
        };
        let right = if self.toc_open {
            "j/k:選ぶ  Enter:飛ぶ  t/Esc:閉じる  q:終了 ".to_string()
        } else {
            "j/k d/u g/G:移動  ]]/[[:見出し  t:目次  e:編集  r:再読込  q:終了 ".to_string()
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
                _ => {} // 組にならなければ2打目を通常のキーとして扱う
            }
        }
        if self.toc_open {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.toc_move(1),
                KeyCode::Char('k') | KeyCode::Up => self.toc_move(-1),
                KeyCode::Char('g') | KeyCode::Home => self.toc_cursor = 0,
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
            KeyCode::Char('e') => return Action::Edit,
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('d') => self.scroll_by(half as isize),
            KeyCode::Char('u') => self.scroll_by(-(half as isize)),
            KeyCode::Char('f') if ctrl => self.scroll_by(self.page_height as isize),
            KeyCode::Char('b') if ctrl => self.scroll_by(-(self.page_height as isize)),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll_by(self.page_height as isize),
            KeyCode::PageUp => self.scroll_by(-(self.page_height as isize)),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = usize::MAX,
            _ => {}
        }
        Action::None
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
        let mut app = App::new(Path::new("test.md"), source);
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
        let mut app = App::new(Path::new("t.md"), &source);
        app.ensure_rendered(40);
        app.scroll = app.rendered.headings[1].line + 2;
        app.ensure_rendered(20);
        assert_eq!(app.scroll, app.rendered.headings[1].line + 2);
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
