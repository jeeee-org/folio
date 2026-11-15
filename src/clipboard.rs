//! クリップボードへ送る。
//!
//! 端末の機能（OSC 52）を常に送り、手元にデスクトップがあればその
//! クリップボードの道具（`wl-copy`・`xclip`・`xsel`）にも渡す。
//! Ptyxisなど VTE系の端末はOSC 52を無視するので、道具の側で届ける。
//! SSH先では道具の環境変数が無いのでOSC 52だけになり、手元の端末へ届く。

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::select;

/// 送った結果。メッセージの言い分けに使う。
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    /// デスクトップのクリップボードの道具で届けた（OSC 52も送った）。
    Desktop(&'static str),
    /// OSC 52だけ送った。端末が対応していれば届く。
    TerminalOnly,
}

/// `text`をクリップボードへ送る。
pub fn copy(text: &str) -> Result<Delivery> {
    let mut out = std::io::stdout();
    out.write_all(select::osc52(text).as_bytes())?;
    out.flush()?;
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    let x11 = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty());
    for (prog, args) in desktop_tools(wayland, x11) {
        if run(prog, args, text) {
            return Ok(Delivery::Desktop(prog));
        }
    }
    Ok(Delivery::TerminalOnly)
}

/// 試す道具の順。Waylandを先にし、X11はXWaylandでも効くので後ろに置く。
pub fn desktop_tools(wayland: bool, x11: bool) -> Vec<(&'static str, &'static [&'static str])> {
    let mut tools: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if wayland {
        tools.push(("wl-copy", &[]));
    }
    if x11 {
        tools.push(("xclip", &["-selection", "clipboard"]));
        tools.push(("xsel", &["--clipboard", "--input"]));
    }
    tools
}

/// 道具に標準入力で渡し、成功したか。道具は裏に残って中身を持ち続けるので、
/// 出力は捨てる（TUIの画面を汚さず、こちらの待ちも塞がない）。
fn run(prog: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let wrote = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
    child.wait().is_ok_and(|s| s.success()) && wrote
}

/// OSC 52だけで送った時に添える注意。VTE系（Ptyxis・GNOME端末）は
/// OSC 52を無視するので、道具を入れれば届くことを伝える。
pub fn hint(delivery: &Delivery, vte: bool) -> &'static str {
    match delivery {
        Delivery::TerminalOnly if vte => {
            "。この端末はOSC 52を受け取らないので、wl-clipboard（Xならxclip）を入れると届きます"
        }
        _ => "",
    }
}

/// いまの端末がVTE系か（VTEは`VTE_VERSION`を立てる）。
pub fn is_vte() -> bool {
    std::env::var_os("VTE_VERSION").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_tries_wl_copy_first() {
        let names: Vec<_> = desktop_tools(true, true).iter().map(|t| t.0).collect();
        assert_eq!(names, ["wl-copy", "xclip", "xsel"]);
    }

    #[test]
    fn x11_only_skips_wl_copy() {
        let names: Vec<_> = desktop_tools(false, true).iter().map(|t| t.0).collect();
        assert_eq!(names, ["xclip", "xsel"]);
    }

    #[test]
    fn no_desktop_sends_only_osc52() {
        assert!(desktop_tools(false, false).is_empty());
    }

    #[test]
    fn hint_only_when_vte_and_no_tool() {
        assert_eq!(hint(&Delivery::Desktop("wl-copy"), true), "");
        assert_eq!(hint(&Delivery::TerminalOnly, false), "");
        assert!(hint(&Delivery::TerminalOnly, true).contains("wl-clipboard"));
    }

    #[test]
    fn missing_tool_is_not_delivered() {
        assert!(!run("folio-no-such-tool", &[], "x"));
    }
}
