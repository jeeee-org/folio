//! 練習の出題。型ごとに「開始位置・模範解答・目標位置」を作る。
//! 目標位置は模範解答をエンジンで実際に動かして求めるので、判定はvimの挙動と必ず一致する。
//! ターミナルには触らない。

use std::fmt;

use crate::vim::{Key, Pos, Vim};

/// 問題の型。履歴で成績を集計する単位でもある。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    GotoLine,
    FindChar,
    TillChar,
    Word,
    WordBack,
    EndOfWord,
    LineEnd,
    LineStart,
    FirstNonBlank,
    Top,
    Bottom,
    Paragraph,
    ParagraphBack,
    Bracket,
    Search,
    Screen,
}

impl Kind {
    pub const ALL: [Kind; 16] = [
        Kind::GotoLine,
        Kind::FindChar,
        Kind::TillChar,
        Kind::Word,
        Kind::WordBack,
        Kind::EndOfWord,
        Kind::LineEnd,
        Kind::LineStart,
        Kind::FirstNonBlank,
        Kind::Top,
        Kind::Bottom,
        Kind::Paragraph,
        Kind::ParagraphBack,
        Kind::Bracket,
        Kind::Search,
        Kind::Screen,
    ];

    /// 履歴ファイルに書く名前（安定させる）。
    pub fn name(self) -> &'static str {
        match self {
            Kind::GotoLine => "goto_line",
            Kind::FindChar => "find_char",
            Kind::TillChar => "till_char",
            Kind::Word => "word",
            Kind::WordBack => "word_back",
            Kind::EndOfWord => "end_of_word",
            Kind::LineEnd => "line_end",
            Kind::LineStart => "line_start",
            Kind::FirstNonBlank => "first_nonblank",
            Kind::Top => "top",
            Kind::Bottom => "bottom",
            Kind::Paragraph => "paragraph",
            Kind::ParagraphBack => "paragraph_back",
            Kind::Bracket => "bracket",
            Kind::Search => "search",
            Kind::Screen => "screen",
        }
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// 人に見せる名前。
    pub fn label(self) -> &'static str {
        match self {
            Kind::GotoLine => "行番号へ（{n}G）",
            Kind::FindChar => "行内の文字へ（f）",
            Kind::TillChar => "行内の文字の直前へ（t）",
            Kind::Word => "単語の頭へ（w）",
            Kind::WordBack => "前の単語へ（b）",
            Kind::EndOfWord => "単語の末尾へ（e）",
            Kind::LineEnd => "行末へ（$）",
            Kind::LineStart => "行頭へ（0）",
            Kind::FirstNonBlank => "最初の文字へ（^）",
            Kind::Top => "先頭行へ（gg）",
            Kind::Bottom => "最終行へ（G）",
            Kind::Paragraph => "次の段落へ（}）",
            Kind::ParagraphBack => "前の段落へ（{）",
            Kind::Bracket => "対応する括弧へ（%）",
            Kind::Search => "検索（/）",
            Kind::Screen => "画面の上/中/下へ（H M L）",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub kind: Kind,
    /// 画面に出す指示
    pub prompt: String,
    pub start: Pos,
    pub target: Pos,
    /// 模範解答（そのまま打てば目標に着く）
    pub answer: String,
}

impl Question {
    /// 模範解答の打鍵数（`\n`は1打）。
    pub fn optimal_keys(&self) -> usize {
        self.answer.chars().count()
    }

    /// 模範解答を人に見せる形（改行はEnterと書く）。
    pub fn answer_display(&self) -> String {
        self.answer.replace('\n', "⏎")
    }

    /// 厳格モードで正解として受け付ける打鍵列。模範解答と、vimで同じ意味の別解。
    pub fn accepted(&self) -> Vec<String> {
        let mut v = vec![self.answer.clone()];
        match self.kind {
            // 5G と 5gg は同じ
            Kind::GotoLine => {
                if let Some(n) = self.answer.strip_suffix('G') {
                    v.push(format!("{n}gg"));
                }
            }
            Kind::Top => v.push("1G".into()),
            _ => {}
        }
        v
    }

    /// ここまでの打鍵が、受け付ける打鍵列のどれかの先頭部分か。
    pub fn accepts_prefix(&self, typed: &str) -> bool {
        self.accepted().iter().any(|a| a.starts_with(typed))
    }
}

/// 依存を増やさないための小さな乱数（xorshift64）。
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn seeded(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn from_time() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Self::seeded(nanos ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// `0..n`の整数。
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }

    /// `lo..=hi`の整数。
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    /// 重みつきで型を選ぶ。
    pub fn pick_weighted(&mut self, weights: &[(Kind, f64)]) -> Kind {
        let total: f64 = weights.iter().map(|(_, w)| w.max(0.0)).sum();
        if total <= 0.0 {
            return *self.pick(&Kind::ALL);
        }
        let mut r = (self.next_u64() % 1_000_000) as f64 / 1_000_000.0 * total;
        for (kind, w) in weights {
            r -= w.max(0.0);
            if r < 0.0 {
                return *kind;
            }
        }
        weights.last().map(|(k, _)| *k).unwrap_or(Kind::Word)
    }
}

/// 出題に使う文書。行と画面の高さを持つ。
pub struct Deck<'a> {
    pub lines: &'a [Vec<char>],
    pub height: usize,
}

impl Deck<'_> {
    /// 型に合う問題を作る。作れない型（括弧が無い等）なら`None`。
    pub fn make(&self, kind: Kind, rng: &mut Rng) -> Option<Question> {
        for _ in 0..40 {
            if let Some(q) = self.try_make(kind, rng) {
                return Some(q);
            }
        }
        None
    }

    /// どれかの型で必ず1問作る（重みで選び、作れなければ別の型へ）。
    pub fn make_any(&self, weights: &[(Kind, f64)], rng: &mut Rng) -> Question {
        for _ in 0..100 {
            let kind = rng.pick_weighted(weights);
            if let Some(q) = self.make(kind, rng) {
                return q;
            }
        }
        // 単語移動はどんな文書でも作れる
        self.make(Kind::Word, rng)
            .or_else(|| self.make(Kind::GotoLine, rng))
            .expect("出題できる文書ではありません")
    }

    fn try_make(&self, kind: Kind, rng: &mut Rng) -> Option<Question> {
        let start = self.random_start(rng, kind)?;
        let line = &self.lines[start.line];
        let (prompt, answer) = match kind {
            Kind::GotoLine => {
                let n = self.lines.len();
                let target = loop {
                    let t = rng.range(1, n);
                    if t != start.line + 1 {
                        break t;
                    }
                };
                (format!("{target}行目へ"), format!("{target}G"))
            }
            Kind::FindChar | Kind::TillChar => {
                // カーソルより右にある、見て分かりやすい文字を1つ選ぶ
                let candidates: Vec<(usize, char)> = line
                    .iter()
                    .enumerate()
                    .skip(start.col + 2)
                    .filter(|(_, c)| !c.is_whitespace() && c.is_ascii_graphic())
                    .map(|(i, c)| (i, *c))
                    .collect();
                if candidates.is_empty() {
                    return None;
                }
                let &(col, ch) = rng.pick(&candidates);
                // その文字が手前に何回出るかで回数を決める（1〜3回目まで）
                let nth = line[start.col + 1..=col]
                    .iter()
                    .filter(|c| **c == ch)
                    .count();
                if nth > 3 {
                    return None;
                }
                let count = if nth == 1 {
                    String::new()
                } else {
                    nth.to_string()
                };
                let ordinal = if nth == 1 {
                    "次の".to_string()
                } else {
                    format!("{nth}個目の")
                };
                if kind == Kind::FindChar {
                    (
                        format!("この行の{ordinal}『{ch}』へ"),
                        format!("{count}f{ch}"),
                    )
                } else {
                    (
                        format!("この行の{ordinal}『{ch}』の直前へ"),
                        format!("{count}t{ch}"),
                    )
                }
            }
            Kind::Word => {
                let n = rng.range(1, 5);
                let count = if n == 1 { String::new() } else { n.to_string() };
                (format!("{n}単語先の頭へ"), format!("{count}w"))
            }
            Kind::WordBack => {
                let n = rng.range(1, 4);
                let count = if n == 1 { String::new() } else { n.to_string() };
                (format!("{n}単語前の頭へ"), format!("{count}b"))
            }
            Kind::EndOfWord => {
                let n = rng.range(1, 3);
                let count = if n == 1 { String::new() } else { n.to_string() };
                (format!("{n}単語先の末尾へ"), format!("{count}e"))
            }
            Kind::LineEnd => ("行末へ".to_string(), "$".to_string()),
            Kind::LineStart => ("行頭へ".to_string(), "0".to_string()),
            Kind::FirstNonBlank => ("この行の最初の文字へ".to_string(), "^".to_string()),
            Kind::Top => ("先頭行へ".to_string(), "gg".to_string()),
            Kind::Bottom => ("最終行へ".to_string(), "G".to_string()),
            Kind::Paragraph => ("次の段落の切れ目（空行）へ".to_string(), "}".to_string()),
            Kind::ParagraphBack => ("前の段落の切れ目（空行）へ".to_string(), "{".to_string()),
            Kind::Bracket => ("対応する括弧へ".to_string(), "%".to_string()),
            Kind::Search => {
                // 別の行にある英単語を選ぶ
                let word = self.random_word(rng, start.line)?;
                (format!("『{word}』を検索して移動"), format!("/{word}\n"))
            }
            Kind::Screen => {
                let (prompt, key) = *rng.pick(&[
                    ("画面の一番上の行へ", "H"),
                    ("画面の中央の行へ", "M"),
                    ("画面の一番下の行へ", "L"),
                ]);
                (prompt.to_string(), key.to_string())
            }
        };
        let target = self.run(start, &answer);
        if target == start {
            return None;
        }
        Some(Question {
            kind,
            prompt,
            start,
            target,
            answer,
        })
    }

    /// 模範解答を実際に動かして目標位置を求める。
    pub fn run(&self, start: Pos, keys: &str) -> Pos {
        let mut vim = self.fresh(start);
        for c in keys.chars() {
            let key = if c == '\n' { Key::Enter } else { Key::Char(c) };
            vim.input(key);
        }
        vim.pos()
    }

    pub fn fresh(&self, start: Pos) -> Vim {
        let text: String = self
            .lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        let mut vim = Vim::new(&text, self.height);
        vim.set_pos(start);
        vim
    }

    /// 型に合った開始位置を選ぶ。
    fn random_start(&self, rng: &mut Rng, kind: Kind) -> Option<Pos> {
        let nonempty: Vec<usize> = (0..self.lines.len())
            .filter(|&i| !self.lines[i].is_empty())
            .collect();
        if nonempty.is_empty() {
            return None;
        }
        for _ in 0..40 {
            let line = *rng.pick(&nonempty);
            let text = &self.lines[line];
            let ok = match kind {
                // 単語系は英字の行だけに出す（日本語の単語境界は未決）
                Kind::Word | Kind::WordBack | Kind::EndOfWord | Kind::FindChar | Kind::TillChar => {
                    text.iter().all(|c| c.is_ascii()) && text.len() >= 12
                }
                Kind::Bracket => text.iter().any(|c| "([{".contains(*c)),
                Kind::LineStart | Kind::LineEnd | Kind::FirstNonBlank => text.len() >= 6,
                _ => true,
            };
            if !ok {
                continue;
            }
            let col = match kind {
                Kind::Bracket => {
                    let opens: Vec<usize> = text
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| "([{".contains(**c))
                        .map(|(i, _)| i)
                        .collect();
                    *rng.pick(&opens)
                }
                Kind::LineStart | Kind::LineEnd => rng.range(1, text.len() - 2),
                Kind::FirstNonBlank => {
                    let first = text.iter().position(|c| !c.is_whitespace()).unwrap_or(0);
                    rng.range(first + 1, text.len() - 1)
                }
                Kind::FindChar | Kind::TillChar | Kind::Word | Kind::EndOfWord => {
                    rng.range(0, text.len() / 3)
                }
                Kind::WordBack => rng.range(text.len() / 2, text.len() - 1),
                _ => rng.range(0, text.len() - 1),
            };
            return Some(Pos::new(line, col));
        }
        None
    }

    /// 開始行より後ろにある、4文字以上の英単語を1つ選ぶ。
    fn random_word(&self, rng: &mut Rng, start_line: usize) -> Option<String> {
        let mut words: Vec<String> = Vec::new();
        for line in self.lines.iter().skip(start_line + 1) {
            let text: String = line.iter().collect();
            for w in text.split(|c: char| !c.is_ascii_alphanumeric()) {
                if w.len() >= 4 && w.chars().all(|c| c.is_ascii_alphabetic()) {
                    words.push(w.to_string());
                }
            }
        }
        if words.is_empty() {
            return None;
        }
        Some(rng.pick(&words).clone())
    }
}

/// 内蔵の練習文。英文・コード・日本語を混ぜ、括弧と段落の切れ目を含む。
pub const SAMPLE_TEXT: &str = r#"fn render(blocks: &[Block], width: u16) -> Vec<Line<'static>> {
    let mut renderer = Renderer::new(width);
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            renderer.blank();
        }
        renderer.block(block);
    }
    renderer.finish()
}

The quick brown fox jumps over the lazy dog while the editor waits.
Practice makes progress, and progress compounds quietly over months.
Move with intent: a single motion should carry you exactly where you look.

folioはターミナルの中で文書を読み、そのまま編集に入るための道具。
段落を飛び、括弧を渡り、語の頭に着地する。目で見た場所へ最短で行く。

impl Cursor {
    pub fn advance(&mut self, count: usize) -> Result<(), MoveError> {
        let target = self.position.saturating_add(count);
        if target >= self.limit {
            return Err(MoveError::OutOfRange { target, limit: self.limit });
        }
        self.position = target;
        Ok(())
    }
}

Speed comes from fewer keystrokes, not faster fingers.
Learn the motion that names the place; stop counting characters.
When in doubt, search: a slash and four letters beat twenty taps of l.

let values = [alpha, beta, gamma, delta, epsilon, zeta, eta, theta];
let total: usize = values.iter().map(|v| v.len()).sum();
println!("{} words, {} letters, {:.1} average", values.len(), total, mean);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn deck_lines() -> Vec<Vec<char>> {
        SAMPLE_TEXT.lines().map(|l| l.chars().collect()).collect()
    }

    #[test]
    fn every_kind_can_be_generated_from_sample_and_answer_reaches_target() {
        let lines = deck_lines();
        let deck = Deck {
            lines: &lines,
            height: 20,
        };
        let mut rng = Rng::seeded(42);
        for kind in Kind::ALL {
            let q = deck
                .make(kind, &mut rng)
                .unwrap_or_else(|| panic!("{kind:?}"));
            assert_ne!(q.start, q.target, "{kind:?}");
            assert_eq!(
                deck.run(q.start, &q.answer),
                q.target,
                "{kind:?} {:?}",
                q.answer
            );
            assert!(!q.prompt.is_empty());
        }
    }

    #[test]
    fn weighted_pick_prefers_heavy_kinds() {
        let mut rng = Rng::seeded(7);
        let weights = vec![(Kind::Top, 0.0), (Kind::Search, 1.0)];
        for _ in 0..50 {
            assert_eq!(rng.pick_weighted(&weights), Kind::Search);
        }
    }

    #[test]
    fn strict_prefix_accepts_answer_and_equivalents_only() {
        let q = Question {
            kind: Kind::GotoLine,
            prompt: String::new(),
            start: Pos::default(),
            target: Pos::new(4, 0),
            answer: "5G".into(),
        };
        assert!(q.accepts_prefix(""));
        assert!(q.accepts_prefix("5"));
        assert!(q.accepts_prefix("5g"));
        assert!(q.accepts_prefix("5gg"));
        assert!(!q.accepts_prefix("4"));
        assert!(!q.accepts_prefix("5j"));
        assert!(!q.accepts_prefix("5Gj"));
    }

    #[test]
    fn kind_names_round_trip() {
        for kind in Kind::ALL {
            assert_eq!(Kind::from_name(kind.name()), Some(kind));
        }
    }
}
