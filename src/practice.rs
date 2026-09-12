//! `folio practice`。vim風の画面の上に問題ボックスを重ね、制限時間内に移動できたらクリア。
//! ターミナル制御はこのモジュールに閉じ込める。出題と判定は`drill`と`vim`。

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use unicode_width::UnicodeWidthChar;

use crate::drill::{Deck, Kind, Question, Rng, SAMPLE_TEXT};
use crate::vim::{Key, Vim};

/// セッションの設定。
#[derive(Debug, Clone)]
pub struct Settings {
    pub count: usize,
    pub time_limit: Duration,
    /// 厳格モード。模範解答（と同等の別解）以外のキーを押した時点で不正解にする
    pub strict: bool,
    /// 問題ボックスに模範解答も出す（見ながら打って体に入れる段階用）
    pub show_answer: bool,
}

/// 1問の結果。
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub kind: Kind,
    pub prompt: String,
    pub answer: String,
    pub cleared: bool,
    /// 厳格モードで違うキーを押して終わった（`cleared`が偽の時だけ意味がある）。打ったキーを持つ
    pub wrong: Option<String>,
    pub seconds: f64,
    pub keys: usize,
    pub optimal: usize,
}

impl Answer {
    /// 得点。クリアで100点、模範解答より多い打鍵1つにつき10点減（下限0）。時間切れは0。
    pub fn score(&self) -> u32 {
        if !self.cleared {
            return 0;
        }
        100u32.saturating_sub(10 * self.keys.saturating_sub(self.optimal) as u32)
    }
}

/// セッション全体の結果。
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub answers: Vec<Answer>,
    pub time_limit_secs: u64,
}

impl Outcome {
    pub fn score(&self) -> u32 {
        self.answers.iter().map(Answer::score).sum()
    }
    pub fn max_score(&self) -> u32 {
        100 * self.answers.len() as u32
    }
    pub fn cleared(&self) -> usize {
        self.answers.iter().filter(|a| a.cleared).count()
    }
}

/// 練習を1セッション実行する。`text`が`None`なら内蔵の練習文。
/// 途中で`Ctrl-c`されたら`None`（記録しない）。
pub fn run(
    text: Option<&Path>,
    settings: &Settings,
    weights: &[(Kind, f64)],
) -> Result<Option<Outcome>> {
    let source = match text {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("{}を読めません", path.display()))?,
        None => SAMPLE_TEXT.to_string(),
    };
    let lines: Vec<Vec<char>> = source.lines().map(|l| l.chars().collect()).collect();
    let mut terminal = ratatui::init();
    let result = Session::new(&lines, settings, weights).run(&mut terminal);
    ratatui::restore();
    result
}

/// 画面の段階。
enum Phase {
    /// 出題中（制限時間が進む）。`typed`は厳格モードの照合用
    Asking {
        started: Instant,
        keys: usize,
        typed: String,
    },
    /// 1問の結果を見せている（少し待つか、キーで次へ）
    Feedback { until: Instant, last: Answer },
    /// 全問終了のまとめ
    Summary,
}

struct Session<'a> {
    lines: &'a [Vec<char>],
    settings: &'a Settings,
    weights: &'a [(Kind, f64)],
    rng: Rng,
    vim: Vim,
    question: Question,
    index: usize,
    phase: Phase,
    outcome: Outcome,
    aborted: bool,
    finished_summary: bool,
    last_height: usize,
}

impl<'a> Session<'a> {
    fn new(lines: &'a [Vec<char>], settings: &'a Settings, weights: &'a [(Kind, f64)]) -> Self {
        // 再現用: FOLIO_PRACTICE_SEED が数値なら同じ出題になる
        let mut rng = std::env::var("FOLIO_PRACTICE_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Rng::seeded)
            .unwrap_or_else(Rng::from_time);
        let deck = Deck { lines, height: 20 };
        let question = deck.make_any(weights, &mut rng);
        let vim = deck.fresh(question.start);
        Self {
            lines,
            settings,
            weights,
            rng,
            vim,
            question,
            index: 0,
            phase: Phase::Asking {
                started: Instant::now(),
                keys: 0,
                typed: String::new(),
            },
            outcome: Outcome {
                answers: Vec::new(),
                time_limit_secs: settings.time_limit.as_secs(),
            },
            aborted: false,
            finished_summary: false,
            last_height: 20,
        }
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> Result<Option<Outcome>> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            if self.aborted {
                return Ok(None);
            }
            if let Phase::Summary = self.phase
                && self.finished_summary
            {
                return Ok(Some(self.outcome));
            }
            self.tick();
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.key(key);
            }
        }
    }

    /// 時間経過で進む処理（時間切れ・結果表示の終了）。
    fn tick(&mut self) {
        match &self.phase {
            Phase::Asking { started, keys, .. } => {
                if started.elapsed() >= self.settings.time_limit {
                    let keys = *keys;
                    self.finish_question(false, None, self.settings.time_limit.as_secs_f64(), keys);
                }
            }
            Phase::Feedback { until, .. } => {
                if Instant::now() >= *until {
                    self.next_question();
                }
            }
            Phase::Summary => {}
        }
    }

    fn key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.aborted = true;
            return;
        }
        match &mut self.phase {
            Phase::Asking {
                started,
                keys,
                typed,
            } => {
                let Some(vk) = to_vim_key(key) else { return };
                *keys += 1;
                typed.push_str(&key_text(vk));
                let secs = started.elapsed().as_secs_f64();
                let keys = *keys;
                // 厳格モード: 模範解答の途中から外れた時点で不正解
                if self.settings.strict && !self.question.accepts_prefix(typed) {
                    let typed = typed.clone();
                    self.finish_question(false, Some(typed), secs, keys);
                    return;
                }
                self.vim.input(vk);
                if self.vim.pos() == self.question.target {
                    self.finish_question(true, None, secs, keys);
                }
            }
            Phase::Feedback { .. } => self.next_question(),
            Phase::Summary => {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Enter | KeyCode::Esc) {
                    self.finished_summary = true;
                }
            }
        }
    }

    fn finish_question(&mut self, cleared: bool, wrong: Option<String>, seconds: f64, keys: usize) {
        let answer = Answer {
            kind: self.question.kind,
            prompt: self.question.prompt.clone(),
            answer: self.question.answer_display(),
            cleared,
            wrong: wrong.map(|t| t.replace('\n', "⏎")),
            seconds,
            keys,
            optimal: self.question.optimal_keys(),
        };
        self.outcome.answers.push(answer.clone());
        let wait = if cleared { 1200 } else { 2500 };
        self.phase = Phase::Feedback {
            until: Instant::now() + Duration::from_millis(wait),
            last: answer,
        };
    }

    fn next_question(&mut self) {
        self.index += 1;
        if self.index >= self.settings.count {
            self.phase = Phase::Summary;
            return;
        }
        let deck = Deck {
            lines: self.lines,
            height: self.vim_height(),
        };
        self.question = deck.make_any(self.weights, &mut self.rng);
        self.vim = deck.fresh(self.question.start);
        self.vim.set_height(self.vim_height());
        self.phase = Phase::Asking {
            started: Instant::now(),
            keys: 0,
            typed: String::new(),
        };
    }

    fn vim_height(&self) -> usize {
        self.last_height.max(1)
    }

    // ---- 描画 ----

    fn draw(&mut self, frame: &mut Frame) {
        let [body, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        self.last_height = body.height as usize;
        self.vim.set_height(body.height as usize);
        self.draw_buffer(frame, body);
        self.draw_status(frame, status);
        match &self.phase {
            Phase::Asking { started, keys, .. } => {
                let remaining = self.settings.time_limit.saturating_sub(started.elapsed());
                self.draw_question_box(frame, body, remaining, *keys);
            }
            Phase::Feedback { last, .. } => self.draw_feedback(frame, body, last),
            Phase::Summary => self.draw_summary(frame, frame.area()),
        }
    }

    /// vim風の本文。行番号・文字・ブロックカーソル。
    fn draw_buffer(&self, frame: &mut Frame, area: Rect) {
        let gutter = self.lines.len().to_string().len().max(3) + 1;
        let top = self.vim.top();
        let cursor = self.vim.pos();
        let mut rows: Vec<Line> = Vec::new();
        for (i, line) in self
            .lines
            .iter()
            .enumerate()
            .skip(top)
            .take(area.height as usize)
        {
            let mut spans = vec![Span::styled(
                format!("{:>width$} ", i + 1, width = gutter - 1),
                Style::new().fg(Color::Yellow),
            )];
            if i == cursor.line {
                let before: String = line[..cursor.col.min(line.len())].iter().collect();
                let at: String = line
                    .get(cursor.col)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| " ".into());
                let after: String = line
                    .get(cursor.col + 1..)
                    .map(|s| s.iter().collect())
                    .unwrap_or_default();
                spans.push(Span::raw(before));
                spans.push(Span::styled(
                    at,
                    Style::new().add_modifier(Modifier::REVERSED),
                ));
                spans.push(Span::raw(after));
            } else {
                spans.push(Span::raw(line.iter().collect::<String>()));
            }
            rows.push(Line::from(spans));
        }
        // vimと同じく、文書の末尾より下は ~ で埋める
        while rows.len() < area.height as usize {
            rows.push(Line::from(Span::styled("~", Style::new().fg(Color::Blue))));
        }
        frame.render_widget(Paragraph::new(Text::from(rows)), area);
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let pos = self.vim.pos();
        let mode = match self.vim.search_input() {
            Some(q) => format!("/{q}"),
            None => format!(
                " NORMAL  {}:{}  {}",
                pos.line + 1,
                pos.col + 1,
                self.vim.pending_text()
            ),
        };
        let right = format!("{}/{}問  Ctrl-c:中断 ", self.index + 1, self.settings.count);
        let gap = (area.width as usize).saturating_sub(
            mode.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() + right.chars().count(),
        );
        let line = Line::from(vec![
            Span::raw(mode),
            Span::raw(" ".repeat(gap)),
            Span::raw(right).dim(),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(Style::new().bg(Color::Indexed(238)).fg(Color::White)),
            area,
        );
    }

    /// 問題ボックス。上部中央に重ね、枠の外は本文をそのまま見せる。
    fn draw_question_box(&self, frame: &mut Frame, body: Rect, remaining: Duration, keys: usize) {
        let area = overlay_area(body, 6);
        frame.render_widget(Clear, area);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(Color::Cyan))
            .title(format!(" 問{} / {} ", self.index + 1, self.settings.count));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [prompt, gauge, info] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        frame.render_widget(
            Paragraph::new(self.question.prompt.clone())
                .style(Style::new().add_modifier(Modifier::BOLD))
                .alignment(Alignment::Center),
            prompt,
        );
        let info_text = if self.settings.show_answer {
            format!("答え: {}    打鍵 {keys}", self.question.answer_display())
        } else {
            format!("打鍵 {keys}")
        };
        let ratio = remaining.as_secs_f64() / self.settings.time_limit.as_secs_f64().max(0.001);
        let color = if ratio > 0.5 {
            Color::Green
        } else if ratio > 0.25 {
            Color::Yellow
        } else {
            Color::Red
        };
        frame.render_widget(
            Gauge::default()
                .ratio(ratio.clamp(0.0, 1.0))
                .gauge_style(Style::new().fg(color).bg(Color::Indexed(236)))
                .label(format!("{:.1}秒", remaining.as_secs_f64())),
            gauge,
        );
        frame.render_widget(
            Paragraph::new(info_text).dim().alignment(Alignment::Center),
            info,
        );
    }

    fn draw_feedback(&self, frame: &mut Frame, body: Rect, last: &Answer) {
        let area = overlay_area(body, 6);
        frame.render_widget(Clear, area);
        let (title, color) = if last.cleared {
            (" クリア ", Color::Green)
        } else if last.wrong.is_some() {
            (" 不正解 ", Color::Red)
        } else {
            (" 時間切れ ", Color::Red)
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(color))
            .title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let mut lines = vec![Line::from(last.prompt.clone()).bold().centered()];
        if last.cleared {
            lines.push(
                Line::from(format!(
                    "{:.1}秒  {}打鍵（模範 {} = {}打鍵）  {}点",
                    last.seconds,
                    last.keys,
                    last.answer,
                    last.optimal,
                    last.score()
                ))
                .centered(),
            );
        } else if let Some(typed) = &last.wrong {
            lines.push(
                Line::from(format!("打ったキー: {typed}    模範解答: {}", last.answer)).centered(),
            );
        } else {
            lines.push(Line::from(format!("模範解答: {}", last.answer)).centered());
        }
        lines.push(Line::from("何かキーで次へ").dim().centered());
        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn draw_summary(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let o = &self.outcome;
        let mut lines = vec![
            Line::from(format!(
                " 結果  {} / {}点   クリア {} / {}問   制限 {}秒",
                o.score(),
                o.max_score(),
                o.cleared(),
                o.answers.len(),
                o.time_limit_secs
            ))
            .bold(),
            Line::from(""),
        ];
        for (i, a) in o.answers.iter().enumerate() {
            let mark = if a.cleared { "○" } else { "×" };
            let detail = if a.cleared {
                format!("{:.1}秒 {}打鍵", a.seconds, a.keys)
            } else if let Some(typed) = &a.wrong {
                format!("不正解({typed})")
            } else {
                "時間切れ".to_string()
            };
            lines.push(Line::from(format!(
                " {mark} {:>2}. {:<28} {:<10} 模範 {:<8} {:>3}点",
                i + 1,
                a.prompt,
                detail,
                a.answer,
                a.score()
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(" qかEnterで終了（結果は履歴に保存されます）").dim());
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
    }
}

/// 本文の上部中央に重ねる枠の範囲。
fn overlay_area(body: Rect, height: u16) -> Rect {
    let width = body.width.saturating_sub(4).clamp(20, 64);
    let x = body.x + (body.width.saturating_sub(width)) / 2;
    let y = body.y + 1;
    Rect::new(x, y, width, height.min(body.height))
}

/// 厳格モードの照合用に、キーを模範解答と同じ表記にする（Enterは改行、Ctrlは`^d`）。
fn key_text(key: Key) -> String {
    match key {
        Key::Char(c) => c.to_string(),
        Key::Ctrl(c) => format!("^{c}"),
        Key::Enter => "\n".to_string(),
        Key::Esc => "\u{1b}".to_string(),
        Key::Backspace => "\u{8}".to_string(),
    }
}

fn to_vim_key(key: KeyEvent) -> Option<Key> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match key.code {
        KeyCode::Char(c) if ctrl => Key::Ctrl(c),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Esc => Key::Esc,
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Left => Key::Char('h'),
        KeyCode::Right => Key::Char('l'),
        KeyCode::Up => Key::Char('k'),
        KeyCode::Down => Key::Char('j'),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_penalizes_extra_keys_and_zeroes_timeouts() {
        let base = Answer {
            kind: Kind::Word,
            prompt: String::new(),
            answer: "3w".into(),
            cleared: true,
            wrong: None,
            seconds: 1.0,
            keys: 2,
            optimal: 2,
        };
        assert_eq!(base.score(), 100);
        assert_eq!(
            Answer {
                keys: 5,
                ..base.clone()
            }
            .score(),
            70
        );
        assert_eq!(
            Answer {
                keys: 30,
                ..base.clone()
            }
            .score(),
            0
        );
        assert_eq!(
            Answer {
                cleared: false,
                ..base
            }
            .score(),
            0
        );
    }

    #[test]
    fn overlay_is_centered_and_bounded() {
        let a = overlay_area(Rect::new(0, 0, 100, 30), 6);
        assert_eq!((a.x, a.y, a.width, a.height), (18, 1, 64, 6));
        let b = overlay_area(Rect::new(0, 0, 30, 4), 6);
        assert_eq!((b.width, b.height), (26, 4));
    }
}
