//! 練習の履歴。`~/.local/share/folio/practice.jsonl`（XDG準拠）に1セッション1行で追記し、
//! 振り返り（`--history`）と出題の重み付けに使う。

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::drill::Kind;
use crate::practice::Outcome;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    /// ローカル時刻（RFC 3339）
    pub at: String,
    pub count: usize,
    pub time_limit_secs: u64,
    pub score: u32,
    pub max_score: u32,
    pub answers: Vec<AnswerRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnswerRecord {
    pub kind: String,
    pub cleared: bool,
    pub seconds: f64,
    pub keys: usize,
    pub optimal: usize,
}

impl Record {
    pub fn from_outcome(outcome: &Outcome) -> Self {
        Self {
            at: chrono::Local::now().to_rfc3339(),
            count: outcome.answers.len(),
            time_limit_secs: outcome.time_limit_secs,
            score: outcome.score(),
            max_score: outcome.max_score(),
            answers: outcome
                .answers
                .iter()
                .map(|a| AnswerRecord {
                    kind: a.kind.name().to_string(),
                    cleared: a.cleared,
                    seconds: a.seconds,
                    keys: a.keys,
                    optimal: a.optimal,
                })
                .collect(),
        }
    }

    pub fn cleared(&self) -> usize {
        self.answers.iter().filter(|a| a.cleared).count()
    }
}

/// 履歴ファイルの置き場所。`$XDG_DATA_HOME/folio/practice.jsonl`、無ければ`~/.local/share/folio/practice.jsonl`。
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
        })?;
    Some(base.join("folio").join("practice.jsonl"))
}

pub fn append(record: &Record) -> Result<PathBuf> {
    let path = path().context("履歴の置き場所が決められません（HOMEが未設定）")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("{}を作れません", dir.display()))?;
    }
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .with_context(|| format!("{}に書けません", path.display()))?;
    Ok(path)
}

/// 履歴を古い順に読む。壊れた行は飛ばす（数を返す）。ファイルが無ければ空。
pub fn load() -> Result<(Vec<Record>, usize)> {
    let Some(path) = path() else {
        return Ok((Vec::new(), 0));
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(e).with_context(|| format!("{}を読めません", path.display())),
    };
    let mut records = Vec::new();
    let mut broken = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Record>(line) {
            Ok(r) => records.push(r),
            Err(_) => broken += 1,
        }
    }
    Ok((records, broken))
}

/// 型ごとの集計。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KindStats {
    pub tries: usize,
    pub cleared: usize,
    pub extra_keys: usize,
    pub seconds: f64,
}

impl KindStats {
    pub fn clear_rate(&self) -> f64 {
        if self.tries == 0 {
            0.0
        } else {
            self.cleared as f64 / self.tries as f64
        }
    }
    pub fn avg_extra_keys(&self) -> f64 {
        if self.cleared == 0 {
            0.0
        } else {
            self.extra_keys as f64 / self.cleared as f64
        }
    }
    pub fn avg_seconds(&self) -> f64 {
        if self.cleared == 0 {
            0.0
        } else {
            self.seconds / self.cleared as f64
        }
    }
}

/// 直近`recent`問（0なら全部）を型ごとに集計する。
pub fn stats(records: &[Record], recent: usize) -> BTreeMap<Kind, KindStats> {
    let mut all: Vec<&AnswerRecord> = records.iter().flat_map(|r| r.answers.iter()).collect();
    if recent > 0 && all.len() > recent {
        all = all.split_off(all.len() - recent);
    }
    let mut map: BTreeMap<Kind, KindStats> = BTreeMap::new();
    for a in all {
        let Some(kind) = Kind::from_name(&a.kind) else {
            continue;
        };
        let s = map.entry(kind).or_default();
        s.tries += 1;
        if a.cleared {
            s.cleared += 1;
            s.extra_keys += a.keys.saturating_sub(a.optimal);
            s.seconds += a.seconds;
        }
    }
    map
}

/// 出題の重み。成績の悪い型（クリア率が低い・余分な打鍵が多い）を重く、
/// まだ出ていない型も少し重くして偏りを避ける。
pub fn weights(records: &[Record]) -> Vec<(Kind, f64)> {
    let stats = stats(records, 200);
    Kind::ALL
        .iter()
        .map(|&kind| {
            let w = match stats.get(&kind) {
                None => 1.5,
                Some(s) => 1.0 + 2.0 * (1.0 - s.clear_rate()) + 0.5 * s.avg_extra_keys().min(2.0),
            };
            (kind, w)
        })
        .collect()
}

/// `--history`の表示。プレーンテキスト。
pub fn report(records: &[Record], broken: usize) -> String {
    let mut out = String::new();
    if records.is_empty() {
        out.push_str("履歴はまだありません。`folio practice`で1セッション試すと記録されます。\n");
        return out;
    }
    out.push_str("直近のセッション（新しい順）\n");
    out.push_str("  日時               問数  制限   点数        クリア\n");
    for r in records.iter().rev().take(20) {
        let when = r.at.get(..16).unwrap_or(&r.at).replace('T', " ");
        out.push_str(&format!(
            "  {when}  {:>4}  {:>3}秒  {:>4}/{:<5}  {:>2}/{}\n",
            r.count,
            r.time_limit_secs,
            r.score,
            r.max_score,
            r.cleared(),
            r.count
        ));
    }
    let total_sessions = records.len();
    let total_q: usize = records.iter().map(|r| r.count).sum();
    out.push_str(&format!(
        "  合計 {total_sessions}セッション {total_q}問\n\n"
    ));

    out.push_str("型ごとの成績（全期間。★は苦手＝クリア率が低いか余分な打鍵が多い）\n");
    out.push_str("  型                              出題  クリア率  余分な打鍵  平均秒\n");
    let st = stats(records, 0);
    for (kind, s) in &st {
        let weak = s.tries >= 2 && (s.clear_rate() < 0.7 || s.avg_extra_keys() >= 1.5);
        out.push_str(&format!(
            "  {}{} {:>4}  {:>6.0}%  {:>9.1}  {:>6.1}\n",
            if weak { "★" } else { " " },
            pad_display(kind.label(), 30),
            s.tries,
            s.clear_rate() * 100.0,
            s.avg_extra_keys(),
            s.avg_seconds()
        ));
    }
    let unseen: Vec<&str> = Kind::ALL
        .iter()
        .filter(|k| !st.contains_key(k))
        .map(|k| k.label())
        .collect();
    if !unseen.is_empty() {
        out.push_str(&format!("  まだ出ていない型: {}\n", unseen.join("、")));
    }
    if broken > 0 {
        out.push_str(&format!("\n（読めない行が{broken}件ありました）\n"));
    }
    out
}

/// 表示幅（全角2桁）で右に空白を足して揃える。
fn pad_display(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let w = text.width();
    format!("{text}{}", " ".repeat(width.saturating_sub(w)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(answers: Vec<(&str, bool, usize, usize)>) -> Record {
        Record {
            at: "2026-09-12T10:00:00+09:00".into(),
            count: answers.len(),
            time_limit_secs: 10,
            score: 0,
            max_score: 0,
            answers: answers
                .into_iter()
                .map(|(k, c, keys, opt)| AnswerRecord {
                    kind: k.into(),
                    cleared: c,
                    seconds: 1.0,
                    keys,
                    optimal: opt,
                })
                .collect(),
        }
    }

    #[test]
    fn stats_aggregate_per_kind() {
        let r = rec(vec![
            ("word", true, 2, 2),
            ("word", false, 0, 2),
            ("top", true, 5, 2),
        ]);
        let st = stats(&[r], 0);
        assert_eq!(st[&Kind::Word].tries, 2);
        assert_eq!(st[&Kind::Word].clear_rate(), 0.5);
        assert_eq!(st[&Kind::Top].avg_extra_keys(), 3.0);
    }

    #[test]
    fn weak_kinds_get_heavier_weights() {
        let r = rec(vec![
            ("word", false, 0, 2),
            ("word", false, 0, 2),
            ("top", true, 2, 2),
            ("top", true, 2, 2),
        ]);
        let w: BTreeMap<Kind, f64> = weights(&[r]).into_iter().collect();
        assert!(w[&Kind::Word] > w[&Kind::Top]);
        assert!(w[&Kind::Search] > w[&Kind::Top]); // 未出題も少し重い
        assert_eq!(w[&Kind::Top], 1.0);
    }

    #[test]
    fn record_round_trips_through_json() {
        let r = rec(vec![("search", true, 6, 6)]);
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Record>(&json).unwrap(), r);
    }

    #[test]
    fn report_mentions_weak_and_unseen_kinds() {
        let r = rec(vec![("word", false, 0, 2), ("word", false, 0, 2)]);
        let text = report(&[r], 1);
        assert!(text.contains("★単語の頭へ"));
        assert!(text.contains("まだ出ていない型"));
        assert!(text.contains("読めない行が1件"));
    }
}
