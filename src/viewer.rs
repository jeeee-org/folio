//! ターミナルに文書を表示し、キー操作でスクロールするビューアー。
//! ターミナル制御はこのモジュールに閉じ込める。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::{DefaultTerminal, Frame};
use unicode_width::UnicodeWidthStr;

use crate::document::{self, Block};
use crate::highlight::Highlighter;
use crate::render::{self, Theme};

/// 折り返し幅の右に空けておく桁数（ぶら下げ句読点の逃げ場）。
const HANG_RESERVE: u16 = 2;

/// `folio render <path>`の本体。TUIを開かず、ANSI付きの行を標準出力に流す。
/// yaziのプレビュー欄（piper経由）や`less -R`から使う。
pub fn render_to_stdout(path: &Path, width: u16) -> Result<()> {
    use std::io::Write;
    let source =
        fs::read_to_string(path).with_context(|| format!("{}を読めません", path.display()))?;
    let blocks = document::parse(&source);
    let lines = render::render_with(&blocks, width, &Theme::default(), Some(&Highlighter::new()));
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for line in &lines {
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
    let mut app = App::load(path)?;
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

struct App {
    path: PathBuf,
    blocks: Vec<Block>,
    lines: Vec<Line<'static>>,
    rendered_width: u16,
    theme: Theme,
    highlighter: Highlighter,
    scroll: usize,
    page_height: usize,
    quit: bool,
    /// ステータス行に出す一時的な知らせ（次のキーで消える）
    message: Option<String>,
}

/// キー操作の結果のうち、ターミナルを触る必要があるもの。
enum Action {
    None,
    Edit,
}

impl App {
    fn load(path: &Path) -> Result<Self> {
        let source =
            fs::read_to_string(path).with_context(|| format!("{}を読めません", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            blocks: document::parse(&source),
            lines: Vec::new(),
            rendered_width: 0,
            theme: Theme::default(),
            highlighter: Highlighter::new(),
            scroll: 0,
            page_height: 1,
            quit: false,
            message: None,
        })
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

    fn draw(&mut self, frame: &mut Frame) {
        let [body, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        let body = body.inner(Margin::new(1, 0));

        // 行頭禁則でぶら下がる句読点（最大2桁）を切らないよう、折り返し幅は描画領域より2桁狭くする
        let wrap_width = body.width.saturating_sub(HANG_RESERVE);
        if wrap_width != self.rendered_width {
            self.lines = render::render_with(
                &self.blocks,
                wrap_width,
                &self.theme,
                Some(&self.highlighter),
            );
            self.rendered_width = wrap_width;
        }
        self.page_height = body.height.max(1) as usize;
        let total = self.lines.len();
        let max_scroll = total.saturating_sub(self.page_height);
        self.scroll = self.scroll.min(max_scroll);
        let end = (self.scroll + self.page_height).min(total);

        let text = Text::from(self.lines[self.scroll..end].to_vec());
        frame.render_widget(Paragraph::new(text), body);
        frame.render_widget(self.status_line(end, total), status);
    }

    fn status_line(&self, end: usize, total: usize) -> Paragraph<'static> {
        let percent = (end * 100).checked_div(total).unwrap_or(100);
        let name = self.path.display().to_string();
        let left = match &self.message {
            Some(message) => format!(" {message}"),
            None => format!(" {name}  {}-{end}/{total} ({percent}%)", self.scroll + 1),
        };
        let right = "j/k:移動  d/u:半頁  g/G:先頭/末尾  e:編集  r:再読込  q:終了 ".to_string();
        let gap = (self.rendered_width as usize + 2 + HANG_RESERVE as usize)
            .saturating_sub(left.width() + right.width());
        let line = Line::from(vec![
            Span::raw(left),
            Span::raw(" ".repeat(gap)),
            Span::raw(right).dim(),
        ]);
        Paragraph::new(line).style(Style::new().bg(Color::Indexed(238)).fg(Color::White))
    }

    fn key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.page_height / 2).max(1);
        match key.code {
            KeyCode::Char('e') => return Action::Edit,
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('d') if ctrl => self.scroll_by(half as isize),
            KeyCode::Char('u') if ctrl => self.scroll_by(-(half as isize)),
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
}
