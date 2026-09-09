# folio

ターミナルの中で文書を読み、そのまま編集に入るための道具。Rust製。

まずはマークダウンを整形して表示するビューアー`folio view <path>`を作り、yaziから開いて使う。段階的に、読みやすさの作り込み・対応形式の追加・vimコマンドの練習モードを足していく。

- 何を作るか: [REQUIREMENTS.md](REQUIREMENTS.md)
- 今どこにいるか: [PROGRESS.md](PROGRESS.md)

## 状態

P1（マークダウンビューアー）を開発中。まだ動くものはない。

## 開発

```bash
cargo run -- view path/to/file.md
cargo test
cargo fmt && cargo clippy -- -D warnings
```
