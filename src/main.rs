use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// ターミナルで文書を読み、そのまま編集に入るための道具。
#[derive(Parser)]
#[command(name = "folio", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// マークダウンファイルを整形して表示する
    View {
        /// 表示するファイル
        path: PathBuf,
    },
    /// TUIを開かず、整形した結果をANSI付きで標準出力に流す（yaziのプレビューやless -R向け）
    Render {
        /// 整形するファイル
        path: PathBuf,
        /// 折り返し幅（桁）。省略時は環境変数COLUMNS、無ければ80
        #[arg(short, long)]
        width: Option<u16>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::View { path } => folio::viewer::run(&path),
        Command::Render { path, width } => {
            let width = width
                .or_else(|| std::env::var("COLUMNS").ok()?.parse().ok())
                .unwrap_or(80);
            folio::viewer::render_to_stdout(&path, width)
        }
    }
}
