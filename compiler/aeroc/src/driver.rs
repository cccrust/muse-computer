//! 把產生的 Rust 寫成 Cargo 專案，交給 cargo/rustc 編譯，並把錯誤包裝回 Aero 診斷。

use crate::codegen::{CodeGen, Output};
use crate::loader::Module;
use crate::diag::{map_rustc_code, Diag};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Emitted {
    pub code: String,
    pub out: Output,
    /// 產生的 Rust 第 n 行（index n-1）→ Aero (檔案編號, 行, 列)；行 = 0 表示無對應
    pub map: Vec<(usize, usize, usize)>,
    /// 各模組的 (路徑, 原始碼)
    pub files: Vec<(String, String)>,
}

pub fn emit_rust(mods: &[Module]) -> Result<Emitted, String> {
    let out = CodeGen::generate(mods);
    // 用 syn 驗證產物一定是合法 Rust，再用 prettyplease 排版
    let file: syn::File =
        syn::parse2(out.tokens.clone()).map_err(|e| format!("內部錯誤：產生了非法的 Rust：{e}"))?;
    let (code, map) = strip_markers(&prettyplease::unparse(&file));
    let files = mods.iter().map(|m| (m.path.clone(), m.src.clone())).collect();
    Ok(Emitted { code, out, map, files })
}

/// 剝除 `#[aero_at(行, 列)]` 標記並建立行號對照表（標記之後的行都歸屬該位置，直到下一個標記）
fn strip_markers(text: &str) -> (String, Vec<(usize, usize, usize)>) {
    let (mut code, mut map, mut cur) = (String::new(), vec![], (0usize, 0usize, 0usize));
    for line in text.lines() {
        let mut line = line.to_string();
        let mut marker_only = false;
        // 標記可能獨占一行，也可能與陳述式同一行（`#[aero_at(1, 2)] return x;`）
        while let Some(pos) = line.find("#[aero_at(") {
            let Some(close) = line[pos..].find(")]") else { break };
            let nums: Vec<usize> = line[pos + "#[aero_at(".len()..pos + close]
                .split(',')
                .filter_map(|x| x.trim().parse().ok())
                .collect();
            if nums.len() == 3 {
                cur = (nums[0], nums[1], nums[2]);
            }
            let indent = line[..pos].to_string();
            let rest = line[pos + close + 2..].trim_start().to_string();
            line = if rest.is_empty() { String::new() } else { format!("{indent}{rest}") };
            if line.is_empty() {
                marker_only = true;
                break;
            }
        }
        if marker_only {
            continue; // 純標記行：丟棄
        }
        code.push_str(&line);
        code.push('\n');
        map.push(cur);
    }
    (code, map)
}

fn cargo_toml(name: &str, deps: &BTreeMap<String, (String, Vec<String>)>) -> String {
    use toml::{Table, Value};
    let mut pkg = Table::new();
    let pkg_name = if deps.contains_key(name) { format!("{name}_app") } else { name.to_string() };
    pkg.insert("name".into(), pkg_name.into());
    pkg.insert("version".into(), "0.1.0".into());
    pkg.insert("edition".into(), "2021".into());

    let mut d = Table::new();
    for (k, (ver, feats)) in deps {
        if feats.is_empty() {
            d.insert(k.clone(), Value::String(ver.clone()));
        } else {
            let mut t = Table::new();
            t.insert("version".into(), ver.clone().into());
            t.insert(
                "features".into(),
                Value::Array(feats.iter().cloned().map(Value::String).collect()),
            );
            d.insert(k.clone(), Value::Table(t));
        }
    }
    // 對應規格書：LLVM LTO / 單一 codegen unit 的極致最佳化
    let mut release = Table::new();
    release.insert("opt-level".into(), 3.into());
    release.insert("lto".into(), true.into());
    release.insert("codegen-units".into(), 1.into());
    let mut profile = Table::new();
    profile.insert("release".into(), Value::Table(release));

    let mut root = Table::new();
    root.insert("package".into(), Value::Table(pkg));
    root.insert("dependencies".into(), Value::Table(d));
    root.insert("profile".into(), Value::Table(profile));
    toml::to_string_pretty(&root).unwrap()
}

pub struct Project {
    pub dir: PathBuf,
    pub bin_name: String,
}

pub fn write_project(dir: &Path, name: &str, code: &str, out: &Output) -> std::io::Result<Project> {
    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::write(dir.join("Cargo.toml"), cargo_toml(name, &out.deps))?;
    std::fs::write(dir.join("src/main.rs"), code)?;
    let bin_name = if out.deps.contains_key(name) { format!("{name}_app") } else { name.to_string() };
    Ok(Project { dir: dir.to_path_buf(), bin_name })
}

pub struct BuildResult {
    pub ok: bool,
    pub diags: Vec<Diag>,
    pub rendered: Vec<String>,
}

/// 執行 `cargo <sub> --message-format=json`，收集 rustc 錯誤
pub fn cargo(
    proj: &Project,
    sub: &str,
    release: bool,
    map: &[(usize, usize, usize)],
    files: &[(String, String)],
) -> std::io::Result<BuildResult> {
    let mut cmd = Command::new("cargo");
    cmd.arg(sub)
        .arg("--manifest-path")
        .arg(proj.dir.join("Cargo.toml"))
        .arg("--message-format=json")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if release {
        cmd.arg("--release");
    }
    let output = cmd.output()?;
    let mut diags = vec![];
    let mut rendered = vec![];
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["reason"] != "compiler-message" || v["message"]["level"] != "error" {
            continue;
        }
        let m = &v["message"];
        let msg = m["message"].as_str().unwrap_or("").to_string();
        if msg.starts_with("aborting due to") {
            continue;
        }
        let rcode = m["code"]["code"].as_str().unwrap_or("");
        let span = m["spans"].as_array().and_then(|s| {
            s.iter().find(|x| x["is_primary"] == true).or_else(|| s.first())
        });
        let (code, explanation, suggestion) = map_rustc_code(rcode, &msg);
        let gl = span.and_then(|s| s["line_start"].as_u64()).unwrap_or(0) as usize;
        let gc = span.and_then(|s| s["column_start"].as_u64()).unwrap_or(0) as usize;
        // 回溯到 .aero 的位置
        let (af, al, ac) = map.get(gl.wrapping_sub(1)).copied().unwrap_or((0, 0, 0));
        let mapped = al > 0;
        diags.push(Diag {
            code,
            message: format!("{msg} (rustc {rcode})"),
            file: if mapped { files[af].0.clone() } else { "src/main.rs (generated)".into() },
            line: if mapped { al } else { gl },
            column: if mapped { ac } else { gc },
            rust_line: gl,
            explanation,
            suggestion,
        });
        rendered.push(m["rendered"].as_str().unwrap_or("").to_string());
    }
    Ok(BuildResult { ok: output.status.success(), diags, rendered })
}

pub fn binary_path(proj: &Project, release: bool) -> PathBuf {
    proj.dir.join("target").join(if release { "release" } else { "debug" }).join(&proj.bin_name)
}
