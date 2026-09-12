//! 設定ファイル`~/.config/folio/config.toml`の読み込み。
//! 無ければ既定で動く。あれば全項目を任意で上書きできる（省略した項目は既定）。

use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

use crate::highlight;
use crate::render::Theme;

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub view: View,
    pub highlight: Highlight,
    pub colors: Colors,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct View {
    /// 本文の折り返しの最大幅（桁）。広い端末では余りを左右に配る
    pub max_width: u16,
    /// 本文の左右の余白（桁）
    pub margin: u16,
}

impl Default for View {
    fn default() -> Self {
        Self {
            max_width: 100,
            margin: 1,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Highlight {
    /// syntectのテーマ名。無い名前なら既定に落ちる
    pub theme: String,
}

impl Default for Highlight {
    fn default() -> Self {
        Self {
            theme: highlight::DEFAULT_THEME.to_string(),
        }
    }
}

/// 色は文字列で書く: 色名（`red` `lightblue` `darkgray`）、256色の番号（`236`）、`#rrggbb`。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    /// 見出しの色。レベル1〜6の順。6つ未満なら最後の色を繰り返す
    pub heading: Vec<String>,
    pub code_block_bg: String,
    pub inline_code_fg: String,
    pub inline_code_bg: String,
    pub link: String,
    pub quote_bar: String,
    pub quote_text: String,
    pub bullet: String,
    pub rule: String,
    pub table_border: String,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            heading: ["yellow", "cyan", "green", "magenta", "white", "white"]
                .map(String::from)
                .to_vec(),
            code_block_bg: "236".into(),
            inline_code_fg: "216".into(),
            inline_code_bg: "236".into(),
            link: "blue".into(),
            quote_bar: "darkgray".into(),
            quote_text: "250".into(),
            bullet: "cyan".into(),
            rule: "darkgray".into(),
            table_border: "darkgray".into(),
        }
    }
}

impl Config {
    /// 設定ファイルの置き場所。`$XDG_CONFIG_HOME/folio/config.toml`、無ければ`~/.config/folio/config.toml`。
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("folio").join("config.toml"))
    }

    /// 設定を読む。ファイルが無ければ既定。あって壊れていればエラー（場所と理由を含む）。
    pub fn load() -> Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err).with_context(|| format!("{}を読めません", path.display())),
        };
        let config: Self =
            Self::parse(&text).with_context(|| format!("{}の設定が不正です", path.display()))?;
        // 色の妥当性もここで確かめ、起動後に落ちないようにする
        config
            .theme()
            .with_context(|| format!("{}の色指定が不正です", path.display()))?;
        Ok(config)
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// 配色を描画用の`Theme`にする。
    pub fn theme(&self) -> Result<Theme> {
        let c = &self.colors;
        let mut theme = Theme::default();
        let bold = Style::new().add_modifier(Modifier::BOLD);
        if c.heading.is_empty() {
            return Err(anyhow!("colors.headingは1色以上必要です"));
        }
        for (i, slot) in theme.heading.iter_mut().enumerate() {
            let name = c.heading.get(i).unwrap_or(c.heading.last().unwrap());
            *slot = bold.fg(color(name, "colors.heading")?);
        }
        theme.code_block = Style::new().bg(color(&c.code_block_bg, "colors.code_block_bg")?);
        theme.inline_code = Style::new()
            .fg(color(&c.inline_code_fg, "colors.inline_code_fg")?)
            .bg(color(&c.inline_code_bg, "colors.inline_code_bg")?);
        theme.link = Style::new()
            .fg(color(&c.link, "colors.link")?)
            .add_modifier(Modifier::UNDERLINED);
        theme.quote_bar = Style::new().fg(color(&c.quote_bar, "colors.quote_bar")?);
        theme.quote_text = Style::new().fg(color(&c.quote_text, "colors.quote_text")?);
        theme.bullet = Style::new().fg(color(&c.bullet, "colors.bullet")?);
        theme.rule = Style::new().fg(color(&c.rule, "colors.rule")?);
        theme.table_border = Style::new().fg(color(&c.table_border, "colors.table_border")?);
        Ok(theme)
    }
}

fn color(name: &str, key: &str) -> Result<Color> {
    Color::from_str(name.trim()).map_err(|_| {
        anyhow!("{key} = \"{name}\" を色として解釈できません（色名・256色の番号・#rrggbb）")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert_eq!(
            Config::default().theme().unwrap().link,
            Theme::default().link
        );
    }

    #[test]
    fn partial_override_keeps_other_defaults() {
        let c = Config::parse(
            "[view]\nmax_width = 80\n\n[colors]\nlink = \"#00ff00\"\nheading = [\"red\"]\n",
        )
        .unwrap();
        assert_eq!(c.view.max_width, 80);
        assert_eq!(c.view.margin, 1);
        let theme = c.theme().unwrap();
        assert_eq!(theme.link.fg, Some(Color::Rgb(0, 255, 0)));
        // 1色しか無ければ全レベルに繰り返す
        assert_eq!(theme.heading[5].fg, Some(Color::Red));
        assert_eq!(theme.code_block.bg, Some(Color::Indexed(236)));
    }

    #[test]
    fn unknown_key_and_bad_color_are_errors() {
        assert!(Config::parse("[view]\nwidth = 1\n").is_err());
        let c = Config::parse("[colors]\nlink = \"not-a-color\"\n").unwrap();
        let err = c.theme().unwrap_err().to_string();
        assert!(err.contains("colors.link"), "{err}");
    }
}
