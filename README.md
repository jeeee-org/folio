# folio

ターミナルの中で文書を読み、そのまま編集に入るための道具。Rust製。

まずはマークダウンを整形して表示するビューアー`folio view <path>`を作り、yaziから開いて使う。段階的に、読みやすさの作り込み・対応形式の追加・vimコマンドの練習モードを足していく。最終的にはファイラーも自作してfolio単体で完結させる予定で、それまでyaziは足場として使う。

- 何を作るか: [REQUIREMENTS.md](REQUIREMENTS.md)
- 今どこにいるか: [PROGRESS.md](PROGRESS.md)

## 使い方

```bash
cargo install --path .        # ~/.cargo/bin/folio
folio view README.md          # TUIで読む。j/k/d/u/g/Gで移動、eでvim、rで再読み込み、qで終了
folio render --width 80 README.md | less -R   # TUIを開かずANSI出力
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
