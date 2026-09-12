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
    /// vimの移動コマンドを時間制限つきで練習する
    Practice {
        /// 練習に使うテキスト（省略時は内蔵の練習文）
        path: Option<PathBuf>,
        /// 問題数
        #[arg(short, long, default_value_t = 10)]
        count: usize,
        /// 1問の制限時間（秒）
        #[arg(short, long, default_value_t = 10)]
        time: u64,
        /// 過去の成績を表示して終了する
        #[arg(long)]
        history: bool,
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
        Command::Practice {
            path,
            count,
            time,
            history,
        } => {
            let (records, broken) = folio::history::load()?;
            if history {
                print!("{}", folio::history::report(&records, broken));
                return Ok(());
            }
            let settings = folio::practice::Settings {
                count: count.max(1),
                time_limit: std::time::Duration::from_secs(time.max(1)),
            };
            let weights = folio::history::weights(&records);
            match folio::practice::run(path.as_deref(), &settings, &weights)? {
                Some(outcome) => {
                    let record = folio::history::Record::from_outcome(&outcome);
                    let saved = folio::history::append(&record)?;
                    println!(
                        "{} / {}点  クリア {} / {}問  → {}に記録（`folio practice --history`で振り返り）",
                        outcome.score(),
                        outcome.max_score(),
                        outcome.cleared(),
                        outcome.answers.len(),
                        saved.display()
                    );
                    Ok(())
                }
                None => {
                    println!("中断しました（記録していません）");
                    Ok(())
                }
            }
        }
    }
}
