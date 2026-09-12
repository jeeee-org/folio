use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use folio::format::Format;

/// ターミナルで文書を読み、そのまま編集に入るための道具。
#[derive(Parser)]
#[command(name = "folio", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// ファイルを整形して表示する（マークダウン・ソースコード・CSV/TSV・テキスト）
    View {
        /// 表示するファイル
        path: PathBuf,
        /// 形式を指定する（省略時は拡張子で判定）: markdown / code / csv / tsv / text
        #[arg(short, long)]
        format: Option<Format>,
    },
    /// TUIを開かず、整形した結果をANSI付きで標準出力に流す（yaziのプレビューやless -R向け）
    Render {
        /// 整形するファイル
        path: PathBuf,
        /// 折り返し幅（桁）。省略時は環境変数COLUMNS、無ければ80
        #[arg(short, long)]
        width: Option<u16>,
        /// 形式を指定する（省略時は拡張子で判定）: markdown / code / csv / tsv / text
        #[arg(short, long)]
        format: Option<Format>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = folio::config::Config::load()?;
    match cli.command {
        Command::View { path, format } => folio::viewer::run(&path, &config, format),
        Command::Render {
            path,
            width,
            format,
        } => {
            let width = width
                .or_else(|| std::env::var("COLUMNS").ok()?.parse().ok())
                .unwrap_or(80);
            folio::viewer::render_to_stdout(&path, width, &config, format)
        }
    }
}
