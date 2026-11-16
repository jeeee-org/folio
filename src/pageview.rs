//! 見た目の表示。画像ファイル1枚、またはPDFの1ページを画面いっぱいに描き、ページを送る。
//! 文字の表示（`viewer`）とは別の画面で、PDFは`i`で行き来する。

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
enum Move {
    By(isize),
    First,
    Last,
}

/// キーを、ページの動きか閉じ方に直す。`pending_g`は`gg`の1打目。
fn key_action(key: KeyEvent, pending_g: &mut bool, can_text: bool) -> Option<Result<Move, Exit>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if std::mem::take(pending_g) && key.code == KeyCode::Char('g') {
        return Some(Ok(Move::First));
    }
    Some(match key.code {
        KeyCode::Char('j')
        | KeyCode::Down
        | KeyCode::Char(' ')
        | KeyCode::PageDown
        | KeyCode::Enter
        | KeyCode::Char('d') => Ok(Move::By(1)),
        KeyCode::Char('f') if ctrl => Ok(Move::By(1)),
        KeyCode::Char('k') | KeyCode::Up | KeyCode::PageUp | KeyCode::Char('u') => Ok(Move::By(-1)),
        KeyCode::Char('b') if ctrl => Ok(Move::By(-1)),
        KeyCode::Char('c') if ctrl => Err(Exit::Quit),
        KeyCode::Char('g') => {
            *pending_g = true;
            return None;
        }
        KeyCode::Home => Ok(Move::First),
        KeyCode::Char('G') | KeyCode::End => Ok(Move::Last),
        KeyCode::Char('i') if can_text => Err(Exit::ToText),
        KeyCode::Char('q') | KeyCode::Esc => Err(Exit::Quit),
        _ => return None,
    })
}

/// 移った先のページ（0始まり、範囲に収める）。
fn moved(page: usize, count: usize, m: Move) -> usize {
    let last = count.saturating_sub(1);
    match m {
        Move::By(d) => page.saturating_add_signed(d).min(last),
        Move::First => 0,
        Move::Last => last,
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
    let mut pending_g = false;
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
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                message = None;
                match key_action(key, &mut pending_g, can_text) {
                    Some(Ok(m)) => {
                        let next = moved(*page, visual.page_count(), m);
                        if next == *page && visual.page_count() > 1 {
                            message = Some(if next == 0 {
                                "最初のページです".into()
                            } else {
                                "最後のページです".into()
                            });
                        }
                        *page = next;
                    }
                    Some(Err(exit)) => return Ok(exit),
                    None if key.code == KeyCode::Char('e') => {
                        message = Some(crate::viewer::NOT_EDITABLE.to_string());
                    }
                    None => {}
                }
            }
            Event::Mouse(MouseEvent { kind, .. }) => match kind {
                MouseEventKind::ScrollDown => {
                    *page = moved(*page, visual.page_count(), Move::By(1))
                }
                MouseEventKind::ScrollUp => *page = moved(*page, visual.page_count(), Move::By(-1)),
                _ => dirty = false,
            },
            Event::Resize(..) => {}
            _ => dirty = false,
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
        "j/k:ページ  gg/G:先頭/末尾  i:文字で読む  q:終了 "
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

    #[test]
    fn keys_move_pages_and_gg_goes_to_first() {
        let mut g = false;
        let mut act = |code| key_action(press(code), &mut g, true);
        assert_eq!(act(KeyCode::Char('j')), Some(Ok(Move::By(1))));
        assert_eq!(act(KeyCode::Char('k')), Some(Ok(Move::By(-1))));
        assert_eq!(act(KeyCode::Char('G')), Some(Ok(Move::Last)));
        assert_eq!(act(KeyCode::Char('g')), None);
        assert_eq!(act(KeyCode::Char('g')), Some(Ok(Move::First)));
        assert_eq!(act(KeyCode::Char('i')), Some(Err(Exit::ToText)));
        assert_eq!(act(KeyCode::Char('q')), Some(Err(Exit::Quit)));
    }

    #[test]
    fn i_does_nothing_for_image_files() {
        let mut g = false;
        assert_eq!(key_action(press(KeyCode::Char('i')), &mut g, false), None);
    }

    #[test]
    fn moves_stay_in_range() {
        assert_eq!(moved(0, 3, Move::By(-1)), 0);
        assert_eq!(moved(2, 3, Move::By(1)), 2);
        assert_eq!(moved(1, 3, Move::Last), 2);
        assert_eq!(moved(2, 3, Move::First), 0);
        assert_eq!(moved(0, 0, Move::By(1)), 0);
    }
}
