//! ターミナルに文書を表示し、キー操作でスクロールするビューアー。
//! ターミナル制御はこのモジュールに閉じ込める。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::{DefaultTerminal, Frame};
use unicode_width::UnicodeWidthStr;

use crate::document::{self, Block};
use crate::render;

/// 折り返し幅の右に空けておく桁数（ぶら下げ句読点の逃げ場）。
const HANG_RESERVE: u16 = 2;

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
    scroll: usize,
    page_height: usize,
    quit: bool,
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
            scroll: 0,
            page_height: 1,
            quit: false,
        })
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            terminal.draw(|frame| self.draw(frame))?;
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.key(key);
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
            self.lines = render::render(&self.blocks, wrap_width);
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
        let left = format!(" {name}  {}-{end}/{total} ({percent}%)", self.scroll + 1);
        let right = "j/k:移動  d/u:半頁  g/G:先頭/末尾  q:終了 ".to_string();
        let gap = (self.rendered_width as usize + 2 + HANG_RESERVE as usize)
            .saturating_sub(left.width() + right.width());
        let line = Line::from(vec![
            Span::raw(left),
            Span::raw(" ".repeat(gap)),
            Span::raw(right).dim(),
        ]);
        Paragraph::new(line).style(Style::new().bg(Color::Indexed(238)).fg(Color::White))
    }

    fn key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.page_height / 2).max(1);
        match key.code {
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
    }

    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
        // 上限はdraw側で行数に合わせて詰める
    }
}
