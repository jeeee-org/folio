# tmuxの設定をREADMEへ（2026-10-02）

## 経緯
- 以前「screen 4.09の中で24ビット色とOSC 52のコピーが通らない」と報告があった件。作業先のサーバーがtmuxへ移り、当面は解決したと共有を受けた（別セッションからの連絡）。
- 先方の確かめ（Debian 13・tmux 3.5a・手元はWindows Terminal + `TERM=xterm-256color`・folio 0.3.0）: 設定なしのtmuxでは24ビット色が256色に落ち（`38;2;255;100;0`→`38;5;202`）、OSC 52は外へ届かない（`set-clipboard`の既定が`external`）。次の3行で両方届き、本物の`folio view`でも24ビット色が届いた。
  - `set -g default-terminal "tmux-256color"`
  - `set -as terminal-features ",xterm*:RGB"`
  - `set -g set-clipboard on`
- folioのOSC 52は素の形（`select.rs`の`osc52`）なので`allow-passthrough`は要らない。

## 変えたこと
- README: クリップボードの節に上の3行を載せ、screenの代わりにこの設定のtmuxを使う案内を足した。
- NOTES.md「ターミナル/TUIの罠」に1行。
- REQUIREMENTS.mdのバックログ（screen対応）に、優先度が下がったことを足した。screen向けのDCS中継は、screenを使う人のための改善として残す。
