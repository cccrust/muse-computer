//! 多檔模組載入：遞迴解析 `import "x.aero" as alias;`，同一檔案只載入一次（允許循環引用）。

use crate::ast::{Decl, Program};
use crate::parser;
use chumsky::error::Simple;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Module {
    pub path: String,
    pub src: String,
    pub prog: Program,
    /// 該模組內符號的前綴：根模組為空字串，其他為 `stem__`
    pub prefix: String,
    /// 別名 → 模組編號
    pub aliases: HashMap<String, usize>,
}

pub enum LoadError {
    Io(String),
    Parse { path: String, src: String, errs: Vec<Simple<char>> },
}

pub fn load(root: &Path) -> Result<Vec<Module>, LoadError> {
    let mut mods: Vec<Module> = vec![];
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    visit(root, &mut mods, &mut index)?;
    // 分配唯一前綴
    let mut used: Vec<String> = vec![String::new()];
    for i in 1..mods.len() {
        let stem: String = Path::new(&mods[i].path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "m".into())
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let mut p = format!("{stem}__");
        if used.contains(&p) {
            p = format!("{stem}{i}__");
        }
        used.push(p.clone());
        mods[i].prefix = p;
    }
    Ok(mods)
}

fn visit(
    path: &Path,
    mods: &mut Vec<Module>,
    index: &mut HashMap<PathBuf, usize>,
) -> Result<usize, LoadError> {
    let canon = std::fs::canonicalize(path)
        .map_err(|e| LoadError::Io(format!("找不到模組 {}: {e}", path.display())))?;
    if let Some(&i) = index.get(&canon) {
        return Ok(i);
    }
    let src = std::fs::read_to_string(&canon)
        .map_err(|e| LoadError::Io(format!("無法讀取 {}: {e}", path.display())))?;
    let display = path.display().to_string();
    let prog = parser::parse(&src)
        .map_err(|errs| LoadError::Parse { path: display.clone(), src: src.clone(), errs })?;

    let id = mods.len();
    index.insert(canon.clone(), id);
    let imports: Vec<(String, Option<String>)> = prog
        .items
        .iter()
        .filter_map(|it| match &it.decl {
            Decl::ImportMod { path, alias } => Some((path.clone(), alias.clone())),
            _ => None,
        })
        .collect();
    mods.push(Module { path: display, src, prog, prefix: String::new(), aliases: HashMap::new() });

    let dir = canon.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut aliases = HashMap::new();
    for (rel, alias) in imports {
        let mut child = dir.join(&rel);
        if child.extension().is_none() {
            child.set_extension("aero");
        }
        let cid = visit(&child, mods, index)?;
        let name = alias.unwrap_or_else(|| {
            child.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        });
        aliases.insert(name, cid);
    }
    mods[id].aliases = aliases;
    Ok(id)
}
