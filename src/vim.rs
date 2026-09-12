//! vimのノーマルモードの移動コマンドを再現するエンジン（編集はしない）。
//! 練習モードの中核で、ターミナルには触らない。単語の境界や行末の扱いはvimの実装
//! （`fwd_word` / `bck_word` / `end_word`）に合わせている。

/// 押されたキー。端末のイベントから変換して渡す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Esc,
    Enter,
    Backspace,
}

/// 1キーを処理した結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// コマンドが完了した（動いていなくても完了は完了。`fz`で見つからない等）
    Done,
    /// 続きのキーを待っている（回数・`g`・`f`の文字・検索の入力中）
    Pending,
    /// 意味のないキーだった
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

#[derive(Debug, Clone)]
pub struct Vim {
    lines: Vec<Vec<char>>,
    pos: Pos,
    /// `j`/`k`で覚えておく桁（vimのcurswant）。`usize::MAX`は行末に張り付く
    want_col: usize,
    /// 表示の最上行と高さ（`H`/`M`/`L`・`Ctrl-d`等に要る）
    top: usize,
    height: usize,
    count: Option<usize>,
    /// 2打鍵目を待つ1打目（`g` `f` `F` `t` `T`）
    prefix: Option<char>,
    /// `;` `,`で繰り返す直前の文字検索
    last_find: Option<(char, char)>,
    /// `n` `N`で繰り返す直前の検索（語と向き）
    last_search: Option<(String, bool)>,
    /// `/` `?`の入力中（語と向き）
    search_input: Option<(String, bool)>,
}

impl Vim {
    pub fn new(text: &str, height: usize) -> Self {
        let mut lines: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        Self {
            lines,
            pos: Pos::default(),
            want_col: 0,
            top: 0,
            height: height.max(1),
            count: None,
            prefix: None,
            last_find: None,
            last_search: None,
            search_input: None,
        }
    }

    pub fn lines(&self) -> &[Vec<char>] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn pos(&self) -> Pos {
        self.pos
    }

    pub fn top(&self) -> usize {
        self.top
    }

    pub fn set_height(&mut self, height: usize) {
        self.height = height.max(1);
        self.scroll_to_cursor();
    }

    /// 入力中の検索（`/`の後の文字列）。表示用。
    pub fn search_input(&self) -> Option<&str> {
        self.search_input.as_ref().map(|(s, _)| s.as_str())
    }

    /// 入力途中のもの（回数や`g`）を表示用に返す。
    pub fn pending_text(&self) -> String {
        let mut s = String::new();
        if let Some(n) = self.count {
            s.push_str(&n.to_string());
        }
        if let Some(p) = self.prefix {
            s.push(p);
        }
        s
    }

    /// カーソルを置く（出題の開始位置用）。範囲外は丸める。
    pub fn set_pos(&mut self, pos: Pos) {
        self.pos.line = pos.line.min(self.lines.len() - 1);
        self.pos.col = pos.col.min(self.line_len(self.pos.line).saturating_sub(1));
        self.want_col = self.pos.col;
        self.scroll_to_cursor();
    }

    fn line_len(&self, line: usize) -> usize {
        self.lines[line].len()
    }

    fn char_at(&self, pos: Pos) -> Option<char> {
        self.lines.get(pos.line)?.get(pos.col).copied()
    }

    pub fn input(&mut self, key: Key) -> Outcome {
        if self.search_input.is_some() {
            return self.search_key(key);
        }
        if let Some(prefix) = self.prefix.take() {
            let outcome = match (prefix, key) {
                ('g', Key::Char('g')) => {
                    let n = self.take_count().unwrap_or(1);
                    self.goto_line(n - 1);
                    Outcome::Done
                }
                ('f' | 'F' | 't' | 'T', Key::Char(c)) => {
                    let n = self.take_count().unwrap_or(1);
                    self.find_char(prefix, c, n);
                    self.last_find = Some((prefix, c));
                    Outcome::Done
                }
                _ => {
                    self.count = None;
                    Outcome::Ignored
                }
            };
            return outcome;
        }
        match key {
            Key::Char(d @ '1'..='9') => {
                self.count = Some(self.count.unwrap_or(0) * 10 + (d as usize - '0' as usize));
                Outcome::Pending
            }
            Key::Char('0') if self.count.is_some() => {
                self.count = Some(self.count.unwrap() * 10);
                Outcome::Pending
            }
            Key::Char(p @ ('g' | 'f' | 'F' | 't' | 'T')) => {
                self.prefix = Some(p);
                Outcome::Pending
            }
            Key::Char(c @ ('/' | '?')) => {
                self.count = None;
                self.search_input = Some((String::new(), c == '/'));
                Outcome::Pending
            }
            Key::Esc => {
                self.count = None;
                Outcome::Done
            }
            Key::Char(c) => {
                let n = self.take_count();
                self.motion(c, n)
            }
            Key::Ctrl(c) => {
                let n = self.take_count();
                self.ctrl_motion(c, n)
            }
            Key::Enter => {
                let n = self.take_count().unwrap_or(1);
                self.down(n);
                self.first_nonblank();
                Outcome::Done
            }
            Key::Backspace => {
                let n = self.take_count().unwrap_or(1);
                for _ in 0..n {
                    self.dec();
                }
                self.want_col = self.pos.col;
                self.scroll_to_cursor();
                Outcome::Done
            }
        }
    }

    fn take_count(&mut self) -> Option<usize> {
        self.count.take()
    }

    fn motion(&mut self, c: char, count: Option<usize>) -> Outcome {
        let n = count.unwrap_or(1);
        match c {
            'h' => {
                self.pos.col = self.pos.col.saturating_sub(n);
                self.want_col = self.pos.col;
            }
            'l' => {
                let last = self.line_len(self.pos.line).saturating_sub(1);
                self.pos.col = (self.pos.col + n).min(last);
                self.want_col = self.pos.col;
            }
            'j' => self.down(n),
            'k' => self.up(n),
            '0' => {
                self.pos.col = 0;
                self.want_col = 0;
            }
            '^' => self.first_nonblank(),
            '$' => {
                self.down(n - 1);
                self.pos.col = self.line_len(self.pos.line).saturating_sub(1);
                self.want_col = usize::MAX;
            }
            'w' => self.fwd_word(n, false),
            'W' => self.fwd_word(n, true),
            'b' => self.bck_word(n, false),
            'B' => self.bck_word(n, true),
            'e' => self.end_word(n, false),
            'E' => self.end_word(n, true),
            'G' => match count {
                Some(n) => self.goto_line(n - 1),
                None => self.goto_line(self.lines.len() - 1),
            },
            '}' => self.paragraph(n, true),
            '{' => self.paragraph(n, false),
            '%' => self.match_bracket(),
            'H' => self.goto_line_keep_view(self.top + (n - 1)),
            'M' => {
                let bottom = (self.top + self.height).min(self.lines.len());
                self.goto_line_keep_view(self.top + (bottom - self.top) / 2);
            }
            'L' => {
                let bottom = (self.top + self.height).min(self.lines.len());
                self.goto_line_keep_view(bottom.saturating_sub(n));
            }
            ';' => {
                if let Some((kind, ch)) = self.last_find {
                    self.find_char(kind, ch, n);
                }
            }
            ',' => {
                if let Some((kind, ch)) = self.last_find {
                    let reverse = match kind {
                        'f' => 'F',
                        'F' => 'f',
                        't' => 'T',
                        _ => 't',
                    };
                    self.find_char(reverse, ch, n);
                }
            }
            'n' => self.search_next(n, true),
            'N' => self.search_next(n, false),
            ' ' => {
                for _ in 0..n {
                    self.inc();
                }
                self.want_col = self.pos.col;
            }
            _ => return Outcome::Ignored,
        }
        self.clamp();
        self.scroll_to_cursor();
        Outcome::Done
    }

    fn ctrl_motion(&mut self, c: char, count: Option<usize>) -> Outcome {
        let half = (self.height / 2).max(1);
        let page = self.height.saturating_sub(2).max(1);
        match c {
            'd' => self.scroll_by(count.unwrap_or(half) as isize),
            'u' => self.scroll_by(-(count.unwrap_or(half) as isize)),
            'f' => self.scroll_by((page * count.unwrap_or(1)) as isize),
            'b' => self.scroll_by(-((page * count.unwrap_or(1)) as isize)),
            _ => return Outcome::Ignored,
        }
        Outcome::Done
    }

    // ---- 基本の移動 ----

    fn down(&mut self, n: usize) {
        self.pos.line = (self.pos.line + n).min(self.lines.len() - 1);
        self.apply_want_col();
    }

    fn up(&mut self, n: usize) {
        self.pos.line = self.pos.line.saturating_sub(n);
        self.apply_want_col();
    }

    fn apply_want_col(&mut self) {
        let last = self.line_len(self.pos.line).saturating_sub(1);
        self.pos.col = self.want_col.min(last);
    }

    fn first_nonblank(&mut self) {
        let line = &self.lines[self.pos.line];
        self.pos.col = line.iter().position(|c| !c.is_whitespace()).unwrap_or(0);
        self.want_col = self.pos.col;
    }

    fn goto_line(&mut self, line: usize) {
        self.pos.line = line.min(self.lines.len() - 1);
        self.first_nonblank();
    }

    fn goto_line_keep_view(&mut self, line: usize) {
        self.pos.line = line.min(self.lines.len() - 1);
        self.first_nonblank();
    }

    /// 画面とカーソルを同じ行数だけ動かす（`Ctrl-d`など）。
    fn scroll_by(&mut self, delta: isize) {
        let max_top = self.lines.len().saturating_sub(1);
        self.top = self.top.saturating_add_signed(delta).min(max_top);
        self.pos.line = self
            .pos
            .line
            .saturating_add_signed(delta)
            .min(self.lines.len() - 1);
        self.first_nonblank();
        self.scroll_to_cursor();
    }

    fn clamp(&mut self) {
        let last = self.line_len(self.pos.line).saturating_sub(1);
        if self.pos.col > last {
            self.pos.col = last;
        }
    }

    fn scroll_to_cursor(&mut self) {
        if self.pos.line < self.top {
            self.top = self.pos.line;
        } else if self.pos.line >= self.top + self.height {
            self.top = self.pos.line + 1 - self.height;
        }
    }

    // ---- 文字単位の前後（vimのinc_cursor / dec_cursor）----

    /// 1文字進む。同じ行なら0、次の行に移ったら1、末尾で動けなければ-1。
    fn inc(&mut self) -> i32 {
        if self.pos.col + 1 < self.line_len(self.pos.line) {
            self.pos.col += 1;
            0
        } else if self.pos.line + 1 < self.lines.len() {
            self.pos.line += 1;
            self.pos.col = 0;
            1
        } else {
            -1
        }
    }

    fn dec(&mut self) -> i32 {
        if self.pos.col > 0 {
            self.pos.col -= 1;
            0
        } else if self.pos.line > 0 {
            self.pos.line -= 1;
            self.pos.col = self.line_len(self.pos.line).saturating_sub(1);
            1
        } else {
            -1
        }
    }

    fn at_empty_line(&self) -> bool {
        self.pos.col == 0 && self.line_len(self.pos.line) == 0
    }

    /// カーソル位置の文字の種類。0=空白、1=記号、2=英数字と`_`、3以上=日本語などの区分。
    fn cls(&self, big: bool) -> u8 {
        match self.char_at(self.pos) {
            None => 0,
            Some(c) => char_class(c, big),
        }
    }

    // ---- 単語移動（vimのsearch.cの移植）----

    fn fwd_word(&mut self, count: usize, big: bool) {
        for _ in 0..count {
            let sclass = self.cls(big);
            if self.inc() == -1 {
                break;
            }
            if sclass != 0 {
                while self.cls(big) == sclass {
                    if self.inc() == -1 {
                        self.want_col = self.pos.col;
                        return;
                    }
                }
            }
            while self.cls(big) == 0 {
                if self.at_empty_line() {
                    break;
                }
                if self.inc() == -1 {
                    break;
                }
            }
        }
        self.want_col = self.pos.col;
    }

    fn bck_word(&mut self, count: usize, big: bool) {
        'outer: for _ in 0..count {
            let mut sclass = self.cls(big);
            if self.dec() == -1 {
                break;
            }
            if self.cls(big) != sclass || sclass == 0 {
                while self.cls(big) == 0 {
                    if self.at_empty_line() {
                        continue 'outer;
                    }
                    if self.dec() == -1 {
                        break 'outer;
                    }
                }
                sclass = self.cls(big);
            }
            loop {
                let before = self.pos;
                if self.dec() == -1 {
                    break;
                }
                if self.cls(big) != sclass {
                    self.pos = before;
                    break;
                }
            }
        }
        self.want_col = self.pos.col;
    }

    fn end_word(&mut self, count: usize, big: bool) {
        for _ in 0..count {
            let sclass = self.cls(big);
            if self.inc() == -1 {
                break;
            }
            if self.cls(big) == sclass && sclass != 0 {
                // 単語の途中にいた: その単語の末尾へ
            } else {
                while self.cls(big) == 0 {
                    if self.inc() == -1 {
                        self.want_col = self.pos.col;
                        return;
                    }
                }
            }
            let sclass = self.cls(big);
            loop {
                let before = self.pos;
                if self.inc() == -1 {
                    break;
                }
                if self.cls(big) != sclass {
                    self.pos = before;
                    break;
                }
            }
        }
        self.want_col = self.pos.col;
    }

    // ---- 行内の文字検索 ----

    fn find_char(&mut self, kind: char, target: char, count: usize) {
        let line = &self.lines[self.pos.line];
        let forward = matches!(kind, 'f' | 't');
        let till = matches!(kind, 't' | 'T');
        let mut col = self.pos.col;
        let mut found = 0;
        loop {
            if forward {
                if col + 1 >= line.len() {
                    return;
                }
                col += 1;
            } else {
                if col == 0 {
                    return;
                }
                col -= 1;
            }
            // `t`で直前の文字に止まっている時、次の`;`は1つ先を探す（vimのcpo-;無しの挙動）
            if line[col] == target {
                found += 1;
                if found == count {
                    break;
                }
            }
        }
        self.pos.col = match (till, forward) {
            (true, true) => col - 1,
            (true, false) => col + 1,
            _ => col,
        };
        self.want_col = self.pos.col;
        self.scroll_to_cursor();
    }

    // ---- 段落と括弧 ----

    /// vimの`findpar`の移植。空行を段落の切れ目とし、文字のある行を通り過ぎた後の
    /// 最初の空行で止まる。端に着いたら先頭行の頭か末尾行の末尾。
    fn paragraph(&mut self, count: usize, forward: bool) {
        let n = self.lines.len();
        let mut curr = self.pos.line;
        for remaining in (0..count).rev() {
            let mut did_skip = false;
            let mut first = true;
            loop {
                if !self.lines[curr].is_empty() {
                    did_skip = true;
                }
                if !first && did_skip && self.lines[curr].is_empty() {
                    break;
                }
                let next = if forward {
                    curr + 1
                } else {
                    curr.wrapping_sub(1)
                };
                if next >= n {
                    if remaining > 0 {
                        // 回数が残っているのに端: vimは動かない
                        return;
                    }
                    break;
                }
                curr = next;
                first = false;
            }
        }
        self.pos.line = curr;
        self.pos.col = 0;
        if forward && curr == n - 1 && !self.lines[curr].is_empty() {
            self.pos.col = self.line_len(curr) - 1;
        }
        self.want_col = self.pos.col;
    }

    fn match_bracket(&mut self) {
        const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
        let line = &self.lines[self.pos.line];
        // カーソル位置かそれより右の最初の括弧
        let Some(start_col) = (self.pos.col..line.len())
            .find(|&c| PAIRS.iter().any(|(o, cl)| line[c] == *o || line[c] == *cl))
        else {
            return;
        };
        let ch = line[start_col];
        let (open, close, forward) = match PAIRS.iter().find(|(o, c)| *o == ch || *c == ch) {
            Some(&(o, c)) => (o, c, ch == o),
            None => return,
        };
        let mut depth = 0i32;
        let mut cur = Pos::new(self.pos.line, start_col);
        loop {
            match self.char_at(cur) {
                Some(c) if c == open => depth += if forward { 1 } else { -1 },
                Some(c) if c == close => depth += if forward { -1 } else { 1 },
                _ => {}
            }
            if depth == 0 {
                self.pos = cur;
                self.want_col = cur.col;
                return;
            }
            let moved = if forward {
                step_forward(&self.lines, &mut cur)
            } else {
                step_backward(&self.lines, &mut cur)
            };
            if !moved {
                return;
            }
        }
    }

    // ---- 検索 ----

    fn search_key(&mut self, key: Key) -> Outcome {
        let (input, forward) = self.search_input.as_mut().unwrap();
        match key {
            Key::Char(c) => {
                input.push(c);
                Outcome::Pending
            }
            Key::Backspace => {
                if input.pop().is_none() {
                    self.search_input = None;
                }
                Outcome::Pending
            }
            Key::Esc => {
                self.search_input = None;
                Outcome::Done
            }
            Key::Enter => {
                let (query, forward) = (input.clone(), *forward);
                self.search_input = None;
                if !query.is_empty() {
                    self.last_search = Some((query, forward));
                    self.search_next(1, true);
                }
                Outcome::Done
            }
            Key::Ctrl(_) => Outcome::Ignored,
        }
    }

    /// 直前の検索を`count`回進める。`same_dir`が偽なら逆向き（`N`）。
    fn search_next(&mut self, count: usize, same_dir: bool) {
        let Some((query, forward)) = self.last_search.clone() else {
            return;
        };
        let forward = forward == same_dir;
        let needle: Vec<char> = query.chars().collect();
        for _ in 0..count {
            if let Some(p) = find_from(&self.lines, self.pos, &needle, forward) {
                self.pos = p;
            }
        }
        self.want_col = self.pos.col;
        self.scroll_to_cursor();
    }
}

/// vimの`utf_class`を簡略化したもの。同じ数字の並びが「単語」になる。
pub fn char_class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        return 0;
    }
    if big {
        return 1;
    }
    if c.is_ascii() {
        return if c.is_ascii_alphanumeric() || c == '_' {
            2
        } else {
            1
        };
    }
    match c as u32 {
        0x3040..=0x309F => 3,                                     // ひらがな
        0x30A0..=0x30FF => 4,                                     // カタカナ
        0x4E00..=0x9FFF | 0x3400..=0x4DBF => 5,                   // 漢字
        0x3000..=0x303F | 0xFF00..=0xFF0F | 0xFF1A..=0xFF20 => 1, // 全角の記号・句読点
        _ => 2,
    }
}

fn step_forward(lines: &[Vec<char>], p: &mut Pos) -> bool {
    if p.col + 1 < lines[p.line].len() {
        p.col += 1;
        true
    } else if p.line + 1 < lines.len() {
        p.line += 1;
        p.col = 0;
        true
    } else {
        false
    }
}

fn step_backward(lines: &[Vec<char>], p: &mut Pos) -> bool {
    if p.col > 0 {
        p.col -= 1;
        true
    } else if p.line > 0 {
        p.line -= 1;
        p.col = lines[p.line].len().saturating_sub(1);
        true
    } else {
        false
    }
}

/// `from`の次（または前）から`needle`を探す。端まで行ったら反対側から続ける。
fn find_from(lines: &[Vec<char>], from: Pos, needle: &[char], forward: bool) -> Option<Pos> {
    let n = lines.len();
    let matches_at = |line: usize, col: usize| lines[line][col..].starts_with(needle);
    for k in 0..=n {
        let line = if forward {
            (from.line + k) % n
        } else {
            (from.line + n - k) % n
        };
        let len = lines[line].len();
        let cols: Box<dyn Iterator<Item = usize>> = match (k, forward) {
            (0, true) => Box::new(from.col + 1..len),
            (0, false) => Box::new((0..from.col.min(len)).rev()),
            (_, true) => Box::new(0..len),
            (_, false) => Box::new((0..len).rev()),
        };
        // 一周して同じ行に戻った時は、開始位置より手前（後ろ）も含めて全部見る
        let cols: Box<dyn Iterator<Item = usize>> = if k == n {
            if forward {
                Box::new(0..=from.col.min(len.saturating_sub(1)))
            } else {
                Box::new((from.col.min(len)..len).rev())
            }
        } else {
            cols
        };
        for col in cols {
            if col < len && matches_at(line, col) {
                return Some(Pos::new(line, col));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "fn main() {\n    let x = foo(1, 2);\n\n    // comment here\n    bar.baz(x);\n}\n\nend of text\n";

    fn vim() -> Vim {
        Vim::new(TEXT, 4)
    }

    fn run(v: &mut Vim, keys: &str) -> (usize, usize) {
        for c in keys.chars() {
            let key = match c {
                '\n' => Key::Enter,
                '\u{1b}' => Key::Esc,
                c => Key::Char(c),
            };
            v.input(key);
        }
        (v.pos().line, v.pos().col)
    }

    #[test]
    fn word_motions_follow_vim_classes() {
        let mut v = vim();
        assert_eq!(run(&mut v, "w"), (0, 3)); // fn → main
        assert_eq!(run(&mut v, "w"), (0, 7)); // main → (
        assert_eq!(run(&mut v, "w"), (0, 10)); // () → {
        assert_eq!(run(&mut v, "w"), (1, 4)); // 次の行の先頭の語
        assert_eq!(run(&mut v, "3w"), (1, 12)); // let x = → foo
        assert_eq!(run(&mut v, "e"), (1, 14)); // foo の末尾
        assert_eq!(run(&mut v, "b"), (1, 12));
        assert_eq!(run(&mut v, "W"), (1, 19)); // foo(1, → 2);
        assert_eq!(run(&mut v, "B"), (1, 12));
    }

    #[test]
    fn w_stops_at_empty_line_and_e_crosses_lines() {
        let mut v = vim();
        run(&mut v, "j$"); // 2行目の末尾 ;
        assert_eq!(run(&mut v, "w"), (2, 0)); // 空行で止まる
        assert_eq!(run(&mut v, "w"), (3, 4)); // 次の語（//）
        assert_eq!(run(&mut v, "e"), (3, 5));
    }

    #[test]
    fn line_motions() {
        let mut v = vim();
        assert_eq!(run(&mut v, "j$"), (1, 21));
        assert_eq!(run(&mut v, "0"), (1, 0));
        assert_eq!(run(&mut v, "^"), (1, 4));
        assert_eq!(run(&mut v, "G"), (7, 0));
        assert_eq!(run(&mut v, "gg"), (0, 0));
        assert_eq!(run(&mut v, "5G"), (4, 4));
        assert_eq!(run(&mut v, "2gg"), (1, 4));
    }

    #[test]
    fn j_k_remember_wanted_column() {
        let mut v = vim();
        run(&mut v, "j$"); // col 21
        assert_eq!(run(&mut v, "j"), (2, 0)); // 空行
        assert_eq!(run(&mut v, "j"), (3, 18)); // 行末に張り付く（$の後）
        run(&mut v, "0fx"); // col 10? 行3は "    // comment here" → xは無い
        let mut v = vim();
        run(&mut v, "j8l"); // col 8
        assert_eq!(run(&mut v, "jj"), (3, 8));
        assert_eq!(run(&mut v, "k"), (2, 0));
        assert_eq!(run(&mut v, "k"), (1, 8));
    }

    #[test]
    fn find_char_and_repeat() {
        let mut v = vim();
        run(&mut v, "j");
        assert_eq!(run(&mut v, "f("), (1, 15));
        assert_eq!(run(&mut v, ";"), (1, 15)); // 2つ目の ( は無い
        assert_eq!(run(&mut v, "0t,"), (1, 16));
        assert_eq!(run(&mut v, "$F("), (1, 15));
        assert_eq!(run(&mut v, "2f)"), (1, 15)); // 2つ目の ) は無いので動かない
        assert_eq!(run(&mut v, "0f)"), (1, 20));
        assert_eq!(run(&mut v, ","), (1, 20)); // 逆向きの ) は無い（col 20 が自分）
    }

    #[test]
    fn paragraph_and_bracket() {
        let mut v = vim();
        assert_eq!(run(&mut v, "}"), (2, 0));
        assert_eq!(run(&mut v, "}"), (6, 0));
        assert_eq!(run(&mut v, "}"), (7, 10)); // 末尾行の末尾
        assert_eq!(run(&mut v, "{"), (6, 0));
        assert_eq!(run(&mut v, "{{"), (0, 0));
        assert_eq!(run(&mut v, "%"), (0, 8)); // 行頭からは最初の括弧 ( → )
        assert_eq!(run(&mut v, "%"), (0, 7));
        assert_eq!(run(&mut v, "$%"), (5, 0)); // { → }
        assert_eq!(run(&mut v, "%"), (0, 10));
        run(&mut v, "j0");
        assert_eq!(run(&mut v, "%"), (1, 20)); // 行内の最初の括弧 ( → )
    }

    #[test]
    fn counts_and_screen_motions() {
        let mut v = vim(); // 高さ4
        assert_eq!(run(&mut v, "3j"), (3, 0)); // jは桁を保つ
        assert_eq!(v.top(), 0);
        assert_eq!(run(&mut v, "j"), (4, 0));
        assert_eq!(v.top(), 1);
        assert_eq!(run(&mut v, "H"), (1, 4));
        assert_eq!(run(&mut v, "L"), (4, 4));
        assert_eq!(run(&mut v, "M"), (3, 4));
        assert_eq!(v.input(Key::Ctrl('d')), Outcome::Done);
        assert_eq!(v.pos().line, 5);
    }

    #[test]
    fn search_wraps_and_n_repeats() {
        let mut v = vim();
        assert_eq!(run(&mut v, "/x\n"), (1, 8));
        assert_eq!(run(&mut v, "n"), (4, 12));
        assert_eq!(run(&mut v, "n"), (7, 9)); // "text" の x
        assert_eq!(run(&mut v, "n"), (1, 8)); // 一周
        assert_eq!(run(&mut v, "N"), (7, 9));
        assert_eq!(run(&mut v, "?foo\n"), (1, 12));
    }

    #[test]
    fn pending_and_ignored() {
        let mut v = vim();
        assert_eq!(v.input(Key::Char('2')), Outcome::Pending);
        assert_eq!(v.pending_text(), "2");
        assert_eq!(v.input(Key::Char('g')), Outcome::Pending);
        assert_eq!(v.pending_text(), "2g");
        assert_eq!(v.input(Key::Char('x')), Outcome::Ignored);
        assert_eq!(v.pending_text(), "");
        assert_eq!(v.input(Key::Char('q')), Outcome::Ignored);
    }
}
