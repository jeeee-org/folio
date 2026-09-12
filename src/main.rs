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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::View { path } => folio::viewer::run(&path),
    }
}
