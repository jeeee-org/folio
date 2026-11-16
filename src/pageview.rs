//! 見た目の表示。画像ファイル1枚、またはPDFの1ページを画面いっぱいに描き、ページを送る。
//! 文字の表示（`viewer`）とは別の画面で、PDFは`i`で行き来する。

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Rect, Size};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui_image::Image;
use unicode_width::UnicodeWidthStr;

use crate::graphics::{self, Visual};

/// 見た目の表示を閉じた理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// 閉じる（呼び出し元へ戻る）
    Quit,
    /// 文字の表示へ切り替える（PDFだけ）
    ToText,
}

/// ページの送り・戻し。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    By(isize),
    First,
    Last,
    /// そのページへ（0始まり）
    To(usize),
}

/// 打ちかけのキー（`gg`・`]]`・`[[`の1打目と、`5G`のような回数）。
#[derive(Debug, Default)]
pub struct Keys {
    pending: Option<char>,
    count: Option<usize>,
}

impl Keys {
    /// キーを、ページの動きか閉じ方に直す。何もしないキーは`None`。
    /// `can_text`はPDF（`i`で文字の表示へ移れる）か。
    pub fn action(&mut self, key: KeyEvent, can_text: bool) -> Option<Result<Move, Exit>> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let pending = self.pending.take();
        if let KeyCode::Char(c @ '0'..='9') = key.code
            && !(c == '0' && self.count.is_none())
        {
            let d = c as usize - '0' as usize;
            self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(d));
            return None;
        }
        let count = self.count.take();
        let by = |sign: isize| Ok(Move::By(sign * count.unwrap_or(1) as isize));
        // 回数つきの`G`・`gg`はそのページへ（vimの行番号と同じ）
        let to_or = |m: Move| Ok(count.map_or(m, |n| Move::To(n.saturating_sub(1))));
        Some(match (pending, key.code) {
            (Some('g'), KeyCode::Char('g')) => to_or(Move::First),
            (Some(']'), KeyCode::Char(']')) => by(1),
            (Some('['), KeyCode::Char('[')) => by(-1),
            (_, KeyCode::Char(c @ ('g' | ']' | '['))) => {
                self.pending = Some(c);
                self.count = count;
                return None;
            }
            (_, KeyCode::Char('f' | 'd')) if ctrl => by(1),
            (_, KeyCode::Char('b' | 'u')) if ctrl => by(-1),
            (_, KeyCode::Char('c')) if ctrl => Err(Exit::Quit),
            (
                _,
                KeyCode::Char('j' | 'l' | ' ' | 'd')
                | KeyCode::Down
                | KeyCode::Right
                | KeyCode::PageDown
                | KeyCode::Enter,
            ) => by(1),
            (
                _,
                KeyCode::Char('k' | 'h' | 'u')
                | KeyCode::Up
                | KeyCode::Left
                | KeyCode::PageUp
                | KeyCode::Backspace,
            ) => by(-1),
            (_, KeyCode::Home) => Ok(Move::First),
            (_, KeyCode::Char('G') | KeyCode::End) => to_or(Move::Last),
            (_, KeyCode::Char('i')) if can_text => Err(Exit::ToText),
            (_, KeyCode::Char('q') | KeyCode::Esc) => Err(Exit::Quit),
            _ => return None,
        })
    }
}

/// 移った先のページ（0始まり、範囲に収める）。
pub fn moved(page: usize, count: usize, m: Move) -> usize {
    let last = count.saturating_sub(1);
    match m {
        Move::By(d) => page.saturating_add_signed(d).min(last),
        Move::First => 0,
        Move::Last => last,
        Move::To(n) => n.min(last),
    }
}

/// ホイールの1回の操作でめくりすぎないための関所。トラックパッドや慣性のあるホイールは
/// 1回の操作で信号を何十も送るので、信号ごとにめくると最初か最後のページまで飛んでしまう。
/// 間を置いて始めた操作の最初の信号で1枚めくり、回し続けている間は`REPEAT`ごとに1枚にする。
#[derive(Debug, Default)]
pub struct WheelGate {
    last_event: Option<Instant>,
    last_flip: Option<Instant>,
}

impl WheelGate {
    /// 操作の切れ目とみなす間隔。
    const GAP: Duration = Duration::from_millis(250);
    /// 回し続けている間にめくる間隔。
    const REPEAT: Duration = Duration::from_millis(500);

    /// `now`に来た信号でめくるか。
    pub fn accept(&mut self, now: Instant) -> bool {
        let fresh = self
            .last_event
            .is_none_or(|t| now.saturating_duration_since(t) >= Self::GAP);
        let repeat = self
            .last_flip
            .is_some_and(|t| now.saturating_duration_since(t) >= Self::REPEAT);
        self.last_event = Some(now);
        if fresh || repeat {
            self.last_flip = Some(now);
        }
        fresh || repeat
    }
}

/// 見た目の表示を動かす。`page`は開くページで、閉じた時のページが入って返る。
pub fn run(
    terminal: &mut DefaultTerminal,
    visual: &mut Visual,
    name: &str,
    page: &mut usize,
) -> Result<Exit> {
    let can_text = visual.is_pdf();
    let mut keys = Keys::default();
    let mut wheel = WheelGate::default();
    let mut message: Option<String> = None;
    *page = (*page).min(visual.page_count().saturating_sub(1));
    let mut dirty = true;
    loop {
        if dirty {
            // 描くのに時間がかかるページは、先に知らせを出してから描く
            let area = body_area(terminal.size()?.into());
            let cached = visual.is_cached(*page, area.as_size());
            if !cached {
                terminal.draw(|f| {
                    draw_status(f, visual, name, *page, Some("描いています…"), can_text)
                })?;
            }
            terminal.draw(|f| draw(f, visual, name, *page, message.as_deref(), can_text))?;
        }
        dirty = true;
        let step = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                message = None;
                match keys.action(key, can_text) {
                    Some(Ok(m)) => Some(m),
                    Some(Err(exit)) => return Ok(exit),
                    None if key.code == KeyCode::Char('e') => {
                        message = Some(crate::viewer::NOT_EDITABLE.to_string());
                        None
                    }
                    None => None,
                }
            }
            Event::Mouse(MouseEvent { kind, .. }) => {
                let sign = match kind {
                    MouseEventKind::ScrollDown => 1,
                    MouseEventKind::ScrollUp => -1,
                    _ => 0,
                };
                if sign != 0 && wheel.accept(Instant::now()) {
                    message = None;
                    Some(Move::By(sign))
                } else {
                    dirty = false;
                    None
                }
            }
            Event::Resize(..) => None,
            _ => {
                dirty = false;
                None
            }
        };
        if let Some(m) = step {
            let next = moved(*page, visual.page_count(), m);
            if next == *page && visual.page_count() > 1 && matches!(m, Move::By(_)) {
                message = Some(if next == 0 {
                    "最初のページです".into()
                } else {
                    "最後のページです".into()
                });
            }
            *page = next;
        }
    }
}

/// 画面のうち、絵を描く場所（下の1行はステータス行）。
fn body_area(screen: Rect) -> Rect {
    let [body, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(screen);
    body
}

fn draw(
    frame: &mut Frame,
    visual: &mut Visual,
    name: &str,
    page: usize,
    message: Option<&str>,
    can_text: bool,
) {
    let body = body_area(frame.area());
    let mut error = None;
    match visual.page(page, Size::new(body.width, body.height)) {
        Ok(protocol) => {
            let at = graphics::centered(protocol, body);
            frame.render_widget(Image::new(protocol), at);
        }
        Err(e) => error = Some(format!("{e:#}")),
    }
    if let Some(e) = &error {
        frame.render_widget(Paragraph::new(e.as_str()).red(), body);
    }
    draw_status(frame, visual, name, page, message, can_text);
}

fn draw_status(
    frame: &mut Frame,
    visual: &Visual,
    name: &str,
    page: usize,
    message: Option<&str>,
    can_text: bool,
) {
    let [_, status] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
    let where_ = match visual.dimensions() {
        Some((w, h)) => format!("{w}×{h}"),
        None => format!("{} / {}ページ", page + 1, visual.page_count()),
    };
    let left = match message {
        Some(m) => format!(" {m}"),
        None => format!(" {name}  {where_}  {}", graphics::protocol_name()),
    };
    let right = if can_text {
        "j/k:めくる  gg/G/数字G:ページへ  i:文字で読む  q:終了 "
    } else {
        "q:終了 "
    };
    let gap = (status.width as usize).saturating_sub(left.width() + right.width());
    let line = Line::from(vec![
        Span::raw(left),
        Span::raw(" ".repeat(gap)),
        Span::raw(right).dim(),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::new().bg(Color::Indexed(238)).fg(Color::White)),
        status,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn feed(keys: &mut Keys, s: &str) -> Option<Result<Move, Exit>> {
        let mut last = None;
        for c in s.chars() {
            last = keys.action(press(KeyCode::Char(c)), true);
        }
        last
    }

    #[test]
    fn keys_move_pages() {
        let mut k = Keys::default();
        assert_eq!(feed(&mut k, "j"), Some(Ok(Move::By(1))));
        assert_eq!(feed(&mut k, "k"), Some(Ok(Move::By(-1))));
        assert_eq!(feed(&mut k, "l"), Some(Ok(Move::By(1))));
        assert_eq!(feed(&mut k, "G"), Some(Ok(Move::Last)));
        assert_eq!(feed(&mut k, "g"), None);
        assert_eq!(feed(&mut k, "g"), Some(Ok(Move::First)));
        // 文字の表示と同じく]]/[[でもページを渡る
        assert_eq!(feed(&mut k, "]]"), Some(Ok(Move::By(1))));
        assert_eq!(feed(&mut k, "[["), Some(Ok(Move::By(-1))));
        assert_eq!(feed(&mut k, "i"), Some(Err(Exit::ToText)));
        assert_eq!(feed(&mut k, "q"), Some(Err(Exit::Quit)));
    }

    #[test]
    fn counts_jump_to_a_page_or_move_several() {
        let mut k = Keys::default();
        assert_eq!(feed(&mut k, "12G"), Some(Ok(Move::To(11))));
        assert_eq!(feed(&mut k, "3gg"), Some(Ok(Move::To(2))));
        assert_eq!(feed(&mut k, "3j"), Some(Ok(Move::By(3))));
        // 回数は1回で消える
        assert_eq!(feed(&mut k, "G"), Some(Ok(Move::Last)));
    }

    #[test]
    fn i_does_nothing_for_image_files() {
        let mut k = Keys::default();
        assert_eq!(k.action(press(KeyCode::Char('i')), false), None);
    }

    #[test]
    fn moves_stay_in_range() {
        assert_eq!(moved(0, 3, Move::By(-1)), 0);
        assert_eq!(moved(2, 3, Move::By(1)), 2);
        assert_eq!(moved(1, 3, Move::Last), 2);
        assert_eq!(moved(2, 3, Move::First), 0);
        assert_eq!(moved(0, 3, Move::To(9)), 2);
        assert_eq!(moved(0, 0, Move::By(1)), 0);
    }

    #[test]
    fn wheel_burst_flips_one_page_then_slowly() {
        let mut gate = WheelGate::default();
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        // 1回の操作で信号が20ms おきに来ても、めくるのは最初の1枚
        assert!(gate.accept(ms(0)));
        for n in 1..20 {
            assert!(!gate.accept(ms(n * 20)), "{n}");
        }
        // 回し続けると500msごとに1枚（0msから数えて最初の500ms以降の信号）
        assert!(gate.accept(ms(400 + 100)));
        assert!(!gate.accept(ms(520)));
        // 手を止めて250ms以上空けた次の操作は、すぐ1枚
        assert!(gate.accept(ms(800)));
    }
}
