//! 診斷：人類可讀（ariadne）＋ 規格書 §7 的 LLM-friendly JSON。

use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::error::{Simple, SimpleReason};
use serde_json::{json, Value};

pub struct Diag {
    pub code: String,
    pub message: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    /// 對應到產生的 Rust 檔行號（0 = 無）
    pub rust_line: usize,
    pub explanation: String,
    pub suggestion: Option<(String, String)>, // (action, patch)
}

impl Diag {
    pub fn to_json(&self) -> Value {
        let mut location = json!({ "file": self.file, "line": self.line, "column": self.column });
        if self.rust_line > 0 {
            location["generated_rust_line"] = json!(self.rust_line);
        }
        let mut v = json!({
            "status": "error",
            "error_code": self.code,
            "message": self.message,
            "location": location,
            "explanation": self.explanation,
        });
        if let Some((action, patch)) = &self.suggestion {
            v["suggestion"] = json!({ "action": action, "patch": patch });
        }
        v
    }
}

fn line_col(src: &str, char_offset: usize) -> (usize, usize) {
    let (mut line, mut col) = (1, 1);
    for (i, c) in src.chars().enumerate() {
        if i == char_offset {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn describe(e: &Simple<char>) -> String {
    match e.reason() {
        SimpleReason::Custom(m) => m.clone(),
        SimpleReason::Unclosed { delimiter, .. } => format!("未閉合的 '{delimiter}'"),
        SimpleReason::Unexpected => {
            let found = e.found().map_or("檔案結尾".to_string(), |c| format!("'{c}'"));
            let mut exp: Vec<String> = e
                .expected()
                .map(|x| x.map_or("檔案結尾".to_string(), |c| format!("'{c}'")))
                .collect();
            exp.sort();
            exp.dedup();
            exp.truncate(8);
            if exp.is_empty() {
                format!("意外的 {found}")
            } else {
                format!("意外的 {found}，預期 {}", exp.join(" / "))
            }
        }
    }
}

pub fn from_parse_errors(file: &str, src: &str, errs: &[Simple<char>]) -> Vec<Diag> {
    errs.iter()
        .map(|e| {
            let (line, column) = line_col(src, e.span().start);
            Diag {
                code: "E0001".into(),
                message: describe(e),
                file: file.into(),
                line,
                column,
                rust_line: 0,
                explanation: "語法錯誤：請檢查區塊是否以 `end` 閉合、每個陳述式是否以 `;` 結尾。".into(),
                suggestion: None,
            }
        })
        .collect()
}

pub fn print_parse_errors(file: &str, src: &str, errs: &[Simple<char>]) {
    for e in errs {
        let span = e.span();
        let _ = Report::build(ReportKind::Error, file, span.start)
            .with_code("E0001")
            .with_message(describe(e))
            .with_label(Label::new((file, span)).with_message(describe(e)).with_color(Color::Red))
            .finish()
            .eprint((file, Source::from(src)));
    }
}

/// 把 rustc 的錯誤碼對應成 Aero 的錯誤碼（Borrow Checker 外包給 rustc 的包裝層）
pub fn map_rustc_code(code: &str, msg: &str) -> (String, String, Option<(String, String)>) {
    match code {
        "E0382" => (
            "E0102".into(),
            "值已被 move 後再次使用（Zero-GC 模式的單一所有權規則）。".into(),
            Some(("改用借用 `&x`，或 `.clone_to_owned()`".into(), "f(&x);".into())),
        ),
        "E0004" => (
            "E0200".into(),
            "match 沒有涵蓋所有情況；請補上缺少的 `case`，或加入 `case _` 作為預設分支。".into(),
            Some(("補上萬用分支".into(), "case _\n  ...".into())),
        ),
        "E0499" => ("E0103".into(), "同一時間只允許一個可變借用 `&mut`。".into(), None),
        "E0502" => ("E0104".into(), "可變借用與不可變借用不可同時存在。".into(), None),
        "E0277" => (
            "E0301".into(),
            "型別未滿足 trait 命題（trait bound 未成立）：該型別缺少編譯器自動衍生所需的結構條件，或泛型參數的邊界 `[T: Trait]` 不夠強。".into(),
            Some((
                "放寬/加強邊界，或補上固有方法讓編譯器自動橋接".into(),
                "fn f[T: Add + Clone](x: T) -> T".into(),
            )),
        ),
        "E0599" => (
            "E0302".into(),
            "找不到方法：該型別沒有此固有方法，且沒有（自動橋接的）trait 提供它。".into(),
            Some((
                "在 `impl Type ... end` 補上同名方法，編譯器會自動橋接".into(),
                "impl Point\n  fn area(&self) -> Int\n    ...\n  end\nend".into(),
            )),
        ),
        "E0405" | "E0433" => (
            "E0303".into(),
            "未知的 trait / 型別名：邊界 `[T: Name]` 中的 Name 既不是內建特徵也不是已宣告的 trait。".into(),
            Some(("先以 `trait Name ... end` 宣告命題".into(), "trait Name\n  ...\nend".into())),
        ),
        "E0505" | "E0597" | "E0515" | "E0106" => (
            "E0105".into(),
            "借用的生命週期不得超過被借用的物件；需要逃逸請改用 `managed` + `gc`。".into(),
            None,
        ),
        _ => ("E0900".into(), format!("底層 rustc 錯誤：{msg}"), None),
    }
}

/// 把（已映射回 .aero 的）診斷用 ariadne 畫在 Aero 原始碼上
pub fn print_diag(file: &str, src: &str, d: &Diag) {
    let chars: Vec<char> = src.chars().collect();
    let (mut i, mut line) = (0usize, 1usize);
    while i < chars.len() && line < d.line {
        if chars[i] == '\n' {
            line += 1;
        }
        i += 1;
    }
    let mut eol = i;
    while eol < chars.len() && chars[eol] != '\n' {
        eol += 1;
    }
    let start = (i + d.column.saturating_sub(1)).min(eol);
    let end = if eol > start { eol } else { (start + 1).min(chars.len().max(1)) };
    let _ = Report::build(ReportKind::Error, file, start)
        .with_code(&d.code)
        .with_message(&d.message)
        .with_label(Label::new((file, start..end)).with_message(&d.explanation).with_color(Color::Red))
        .finish()
        .eprint((file, Source::from(src)));
}
