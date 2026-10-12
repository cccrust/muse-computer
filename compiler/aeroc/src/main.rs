mod ast;
mod codegen;
mod diag;
mod driver;
mod loader;
mod parser;

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::process::{Command, ExitCode};

#[derive(Parser)]
#[command(name = "aeroc", version, about = "Aero → Rust 編譯器")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args)]
struct Opts {
    /// 入口 .aero 原始檔（其 import 的模組會自動載入）
    file: PathBuf,
    /// 以規格書 §7 的 JSON 格式輸出錯誤（給 LLM 自我修復用）
    #[arg(long)]
    json: bool,
    #[arg(long)]
    release: bool,
    /// 產生的 Cargo 專案目錄（預設 target/aero/<檔名>）
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 只印出轉譯後的 Rust 程式碼
    Emit { file: PathBuf },
    /// 轉譯 + cargo check（讓 rustc 當 Borrow Checker）
    Check(Opts),
    /// 轉譯 + cargo build
    Build(Opts),
    /// 轉譯 + 編譯 + 執行
    Run(Opts),
}

fn load(file: &PathBuf, json: bool) -> Result<Vec<loader::Module>, ExitCode> {
    match loader::load(file) {
        Ok(m) => Ok(m),
        Err(loader::LoadError::Io(e)) => {
            eprintln!("{e}");
            Err(ExitCode::from(2))
        }
        Err(loader::LoadError::Parse { path, src, errs }) => {
            if json {
                let ds = diag::from_parse_errors(&path, &src, &errs);
                println!("{}", serde_json::to_string_pretty(&ds[0].to_json()).unwrap());
            } else {
                diag::print_parse_errors(&path, &src, &errs);
            }
            Err(ExitCode::from(1))
        }
    }
}

fn pipeline(o: &Opts, sub: &str, run: bool) -> ExitCode {
    let mods = match load(&o.file, o.json) {
        Ok(x) => x,
        Err(c) => return c,
    };
    let em = match driver::emit_rust(&mods) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(3);
        }
    };
    let stem: String = o
        .file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "aero_app".into())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let dir = o.out_dir.clone().unwrap_or_else(|| PathBuf::from("target/aero").join(&stem));
    let proj = match driver::write_project(&dir, &stem, &em.code, &em.out) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("寫入專案失敗: {e}");
            return ExitCode::from(3);
        }
    };
    let res = match driver::cargo(&proj, sub, o.release, &em.map, &em.files) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("無法執行 cargo: {e}");
            return ExitCode::from(3);
        }
    };
    if !res.ok {
        if o.json {
            for d in &res.diags {
                println!("{}", serde_json::to_string_pretty(&d.to_json()).unwrap());
            }
        } else {
            for (d, r) in res.diags.iter().zip(&res.rendered) {
                match em.files.iter().find(|(p, _)| *p == d.file) {
                    Some((p, src)) => diag::print_diag(p, src, d),
                    None => eprintln!("{r}"),
                }
            }
        }
        return ExitCode::from(1);
    }
    if run {
        let status = Command::new(driver::binary_path(&proj, o.release)).status();
        return match status {
            Ok(s) if s.success() => ExitCode::SUCCESS,
            _ => ExitCode::from(1),
        };
    }
    eprintln!("✔ 完成：{}", dir.display());
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    match Cli::parse().cmd {
        Cmd::Emit { file } => {
            let mods = match load(&file, false) {
                Ok(x) => x,
                Err(c) => return c,
            };
            match driver::emit_rust(&mods) {
                Ok(em) => {
                    print!("{}", em.code);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::from(3)
                }
            }
        }
        Cmd::Check(o) => pipeline(&o, "check", false),
        Cmd::Build(o) => pipeline(&o, "build", false),
        Cmd::Run(o) => pipeline(&o, "build", true),
    }
}
