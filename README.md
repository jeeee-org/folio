# folio

ターミナルの中で文書を読み、そのまま編集に入るための道具。Rust製。

まずはマークダウンを整形して表示するビューアー`folio view <path>`を作り、yaziから開いて使う。段階的に、読みやすさの作り込み・対応形式の追加・vimコマンドの練習モードを足していく。最終的にはファイラーも自作してfolio単体で完結させる予定で、それまでyaziは足場として使う。

- 何を作るか: [REQUIREMENTS.md](REQUIREMENTS.md)
- 今どこにいるか: [PROGRESS.md](PROGRESS.md)

## 使い方

```bash
cargo install --path .        # ~/.cargo/bin/folio
folio view README.md          # TUIで読む
folio render --width 80 README.md | less -R   # TUIを開かずANSI出力
```

キーは3層。移動と検索はvim準拠、ページャ的な操作はlessと互換、アプリ操作はvimの移動キーを避ける。

| 操作 | キー |
|---|---|
| 1行／半ページ／1ページ | `j` `k`／`d` `u`（`Ctrl-d` `Ctrl-u`）／`Space` `Ctrl-f` `Ctrl-b` |
| 先頭／末尾 | `gg`／`G` |
| 次／前の見出し | `]]`／`[[` |
| 目次ペイン | `t`で開閉。中では`j` `k` `gg` `G`で選び`Enter`で飛ぶ |
| 検索 | `/`で入力、`n` `N`で次／前、`Esc`で解除。大文字を含む時だけ大小を区別 |
| URLの表示切替 | `U` |
| vimで編集／再読込 | `e`／`r` |
| 終了 | `q`（`Esc`は解除するものが無い時だけ終了） |

## 設定

`~/.config/folio/config.toml`（`$XDG_CONFIG_HOME`があればその下）。無ければ既定で動き、書いた項目だけ上書きされる。以下は既定値。

```toml
[view]
max_width = 100     # 本文の折り返しの最大幅（桁）。広い端末では余りを左右に配る
margin = 1          # 本文の左右の余白（桁）

[highlight]
theme = "base16-ocean.dark"   # コードブロックの配色（syntectのテーマ名）

[colors]            # 色名（red, lightblue, darkgray…）、256色の番号（"236"）、"#rrggbb"
heading = ["yellow", "cyan", "green", "magenta", "white", "white"]   # 見出しレベル1〜6
code_block_bg = "236"
inline_code_fg = "216"
inline_code_bg = "236"
link = "blue"
quote_bar = "darkgray"
quote_text = "250"
bullet = "cyan"
rule = "darkgray"
table_border = "darkgray"
url = "darkgray"        # Uキーで表示するURL
```

## yaziとの連携

`~/.config/yazi/yazi.toml`に足す。プレビュー欄は[piper.yazi](https://github.com/yazi-rs/plugins/tree/main/piper.yazi)経由。

```toml
[[plugin.prepend_previewers]]
url = "*.md"
run = 'piper -- folio render --width "$w" "$1"'

[opener]
folio = [
  { run = 'folio view "$1"', block = true, desc = "folioで読む" },
]

[open]
prepend_rules = [
  { url = "*.md", use = ["folio", "edit"] },
]
```

## 状態

P1（マークダウンビューアー＋yazi連携＋vim往復）の完了条件を満たした。次は仕上げと読みやすさの作り込み（P2）。

## 開発

```bash
cargo run -- view path/to/file.md
cargo test
cargo fmt && cargo clippy -- -D warnings
```
