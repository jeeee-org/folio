//! 描画後の行に対する文字列検索。中核の一部で、ターミナルには触らない。
//!
//! 対象は描画後の本文（記号を除いた、読んでいる文字列）。原文を対象にすると
//! `**`や`#`にヒットして画面と食い違うため。

use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// 1件のヒット。`line`は行の添字、`start`/`end`は行内の文字（char）位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// 行の列から`query`を探す。大文字を含む時だけ大小を区別する（smartcase）。
pub fn find_all(lines: &[Line<'_>], query: &str) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }
    let sensitive = query.chars().any(char::is_uppercase);
    let needle: Vec<char> = query.chars().map(|c| fold(c, sensitive)).collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let hay: Vec<char> = plain(line).chars().map(|c| fold(c, sensitive)).collect();
        let mut from = 0;
        while from + needle.len() <= hay.len() {
            match hay[from..].windows(needle.len()).position(|w| w == needle) {
                Some(off) => {
                    let start = from + off;
                    found.push(Match {
                        line: i,
                        start,
                        end: start + needle.len(),
                    });
                    from = start + needle.len();
                }
                None => break,
            }
        }
    }
    found
}

fn fold(c: char, sensitive: bool) -> char {
    if sensitive {
        c
    } else {
        // 1文字→1文字の近似（'İ'のような多文字化は無視する）。添字を揃えるため
        c.to_lowercase().next().unwrap_or(c)
    }
}

pub fn plain(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// 行に、文字位置の範囲ごとの装飾を重ねる。スパンの境界をまたぐ範囲も扱う。
pub fn highlight(line: &Line<'static>, ranges: &[(usize, usize, Style)]) -> Line<'static> {
    if ranges.is_empty() {
        return line.clone();
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut pos = 0; // 行内の文字位置
    for span in &line.spans {
        let chars: Vec<char> = span.content.chars().collect();
        let mut buf = String::new();
        let mut buf_style: Option<Style> = None;
        for (k, ch) in chars.iter().enumerate() {
            let idx = pos + k;
            let extra = ranges
                .iter()
                .filter(|(s, e, _)| *s <= idx && idx < *e)
                .fold(None, |acc: Option<Style>, (_, _, st)| {
                    Some(acc.map_or(*st, |a| a.patch(*st)))
                });
            let style = match extra {
                Some(st) => span.style.patch(st),
                None => span.style,
            };
            if buf_style != Some(style) {
                if !buf.is_empty() {
                    out.push(Span::styled(std::mem::take(&mut buf), buf_style.unwrap()));
                }
                buf_style = Some(style);
            }
            buf.push(*ch);
        }
        if !buf.is_empty() {
            out.push(Span::styled(buf, buf_style.unwrap()));
        }
        pos += chars.len();
    }
    let mut result = Line::from(out);
    result.style = line.style;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    fn lines(v: &[&str]) -> Vec<Line<'static>> {
        v.iter().map(|s| Line::from(s.to_string())).collect()
    }

    #[test]
    fn finds_all_occurrences_case_insensitively() {
        let m = find_all(&lines(&["Foo foo", "bar", "fOO"]), "foo");
        assert_eq!(
            m,
            vec![
                Match {
                    line: 0,
                    start: 0,
                    end: 3
                },
                Match {
                    line: 0,
                    start: 4,
                    end: 7
                },
                Match {
                    line: 2,
                    start: 0,
                    end: 3
                },
            ]
        );
    }

    #[test]
    fn uppercase_in_query_makes_it_sensitive() {
        let m = find_all(&lines(&["Foo foo"]), "Foo");
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn positions_are_in_chars_not_bytes() {
        let m = find_all(&lines(&["あいう検索"]), "検索");
        assert_eq!(
            m,
            vec![Match {
                line: 0,
                start: 3,
                end: 5
            }]
        );
    }

    #[test]
    fn highlight_splits_spans_at_range_edges() {
        let line = Line::from(vec![
            Span::raw("ab"),
            Span::styled("cd", Style::new().fg(Color::Blue)),
        ]);
        let hl = Style::new().add_modifier(Modifier::REVERSED);
        let out = highlight(&line, &[(1, 3, hl)]);
        let parts: Vec<(String, bool)> = out
            .spans
            .iter()
            .map(|s| {
                (
                    s.content.to_string(),
                    s.style.add_modifier.contains(Modifier::REVERSED),
                )
            })
            .collect();
        assert_eq!(
            parts,
            vec![
                ("a".into(), false),
                ("b".into(), true),
                ("c".into(), true),
                ("d".into(), false),
            ]
        );
        assert_eq!(out.spans[2].style.fg, Some(Color::Blue));
    }
}
