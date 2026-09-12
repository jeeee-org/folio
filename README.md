# folio

ターミナルの中で文書を読み、そのまま編集に入るための道具。Rust製。

マークダウンを整形して表示するビューアー`folio view <path>`をyaziから開いて使う。プレーンテキスト・ソースコード（ハイライト付き）・CSV/TSV（表）も読める。段階的に、読みやすさの作り込み・対応形式の追加・vimコマンドの練習モードを足していく。最終的にはファイラーも自作してfolio単体で完結させる予定で、それまでyaziは足場として使う。

- 何を作るか: [REQUIREMENTS.md](REQUIREMENTS.md)
- 今どこにいるか: [PROGRESS.md](PROGRESS.md)

## 使い方

```bash
cargo install --path .        # ~/.cargo/bin/folio
folio view README.md          # TUIで読む。形式は拡張子で判定（md / csv / tsv / ソースコード / それ以外はテキスト）
folio view data.txt --format csv              # 判定を上書き（markdown / code / csv / tsv / text）
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

## vimの移動を練習する

```bash
folio practice                    # 内蔵の練習文で10問、1問10秒
folio practice --count 30 --time 6   # 長め・厳しめ
folio practice src/main.rs        # 好きなファイルで
folio practice --show             # 模範解答を見ながら打つ（初見の型向け）
folio practice --strict           # 厳格な判定（既定なので省略可）
folio practice --loose            # 緩い判定（違うキーを押しても続けられる）
folio practice --history          # 過去の点数と型ごとの弱点
```

vim風の画面の上に問題（「3単語先の頭へ」「対応する括弧へ」「『render』を検索して移動」…）が出る。既定は**厳格**で、模範解答（`5G`と`5gg`のような同等の別解を含む）と違うキーを押した時点で不正解になり、打ったキーと答えが出る。初見の型は`--show`で答えを見ながら打ち、覚えたら厳格で隠して打つ、慣れたら`--loose`で総合力、の3段階。`--loose`にすると違うキーを押しても続けられ、制限時間内にカーソルが目標に着けばクリア。クリア100点、模範解答より多い打鍵1つにつき10点減、不正解・時間切れ0点。出題は16種で、`h j k l`・`w b e`・`f t ; ,`・`0 ^ $`・`gg G`・`{ }`・`%`・`H M L`・`Ctrl-d/u/f/b`・`/ ? n N`・回数指定が使える。結果は`~/.local/share/folio/practice.jsonl`に残り、成績の悪い型が多めに出るようになる。`Ctrl-c`で中断（記録しない）。

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
  { url = "*.{csv,tsv}", use = ["folio", "edit"] },
]
```

## 状態

P1（ビューアー・yazi連携・vim往復）、P2（目次・検索・表・設定）、P3（テキスト・ソースコード・CSV/TSV）、P4（vimの移動の練習モード）まで完了。次はP5（ファイラー本体の自作）。

## 開発

```bash
cargo run -- view path/to/file.md
cargo test
cargo fmt && cargo clippy -- -D warnings
```
