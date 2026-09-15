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

## ファイルを選んで読む（ファイラーの第一歩）

```bash
folio browse            # カレントから。左に一覧、右にプレビュー
folio browse ~/notes    # 開始ディレクトリを指定
```

| 操作 | キー |
|---|---|
| 一覧の移動 | `j` `k` `gg` `G`、半ページ`Ctrl-d` `Ctrl-u` |
| 親へ／ディレクトリに入る | `h`／`l` `Enter` |
| ファイルを読む | `Enter`でビューアーが全画面。`q`で一覧に戻る |
| vimで編集 | `e` |
| 新規作成 | `a`。名前を入力、末尾`/`でディレクトリ（`a/b/c.md`のように途中も作る）。既存名は拒む |
| 改名 | `r`。今の名前を編集。既存名への上書きは拒む |
| ごみ箱へ | `d`。`y`で実行。`~/.local/share/Trash`へ（XDG Trash。別ファイルシステムはコピーしてから消す） |
| コピー／移動 | `y`（コピー元）または`x`（移動元）で覚え、行き先で`p`。同名があれば`y/N`で聞いてから上書き。`Esc`で忘れる |
| 外部アプリで開く | `o`（`wslview`があればそれ、無ければ`xdg-open`。画像・PDFなど） |
| 隠しファイル／再読込 | `.`／`R` |
| プレビューのスクロール | `J` `K` |
| 終了 | `q` |

ファイル操作のキーはyaziと同じ。1MBを超えるファイルとUTF-8でないファイルはプレビューしない。複数選択・完全削除・undoはまだ無い。

マウスでも辿れる。

| 操作 | マウス |
|---|---|
| 選ぶ（右にプレビュー） | 一覧の項目をクリック |
| ディレクトリに入る／ファイルを全画面で読む | 一覧の項目をダブルクリック |
| 親へ | 一覧の上端のパスの行をクリック |
| パスをコピー | 一覧の項目を右クリックし、メニューの「絶対パス」「相対パス」「ファイル名」をクリック（`j` `k`と`Enter`でも選べる）。相対パスはgitリポジトリのルートから、リポジトリの外なら起動したフォルダから |
| プレビューに出ているフォルダの中身を開く | 項目を1回クリック。フォルダならその中へ入り、ファイルならそのフォルダへ移って全画面で読む |
| スクロール | ホイール（1回で5行）。ポインタが一覧の上なら一覧、プレビューの上ならプレビュー、全画面では本文が動く |
| 文字をコピー | ファイルのプレビューか全画面の本文をドラッグして選び、`Ctrl-c`でクリップボードへ。`Esc`で選択を解除。上下にはみ出すと送りながら広げる |

ドラッグで選ばれるのは文字のある所だけで、中央寄せの余白と行末の空白は入らない。コピーされるのは見た目どおりの本文（`#`や`**`は付かない）で、幅で折り返された行は1行につなぎ、段落や表・コードの行は改行のまま。`folio view`を単体で開いた時も同じ。選んでいない時の`Ctrl-c`は今までどおり終了。

クリップボードへは端末の機能（OSC 52）で送るので、Windows Terminalならそのまま使える。tmuxの中では`set -g set-clipboard on`が要る。folioがマウスを受け取っている間、端末そのものの文字選択は**`Shift`を押しながらドラッグ**（`Alt`も足すと矩形）で使える。

### yaziから置き換える

日常の入口を`folio browse`にするには、シェルに関数を1つ置く（終了したディレクトリに移動する。yaziと同じ`--cwd-file`方式）。`.zshrc`や`.bashrc`に:

```bash
fb() { local tmp; tmp=$(mktemp); folio browse --cwd-file "$tmp" "$@"; local d; d=$(cat "$tmp"); rm -f "$tmp"; [ -n "$d" ] && cd "$d"; }
```

yaziの設定（`yazi.toml`のopenerとpreviewer）は消さなくてよい。yaziを開けばこれまでどおりfolioで読めるので、戻りたい時にそのまま戻れる。

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

P1（ビューアー・yazi連携・vim往復）、P2（目次・検索・表・設定）、P3（テキスト・ソースコード・CSV/TSV）、P4（vimの移動の練習モード）、P5（`folio browse`: 一覧・移動・プレビュー）、P6（作成・改名・ごみ箱）、P7（コピー・移動・外部アプリ、yaziの置き換え）まで完了。P8（ファイラーのマウス操作）は実装済みで実機確認待ち。日常の入口はfolioへ。

## 開発

```bash
cargo run -- view path/to/file.md
cargo test
cargo fmt && cargo clippy -- -D warnings
```
