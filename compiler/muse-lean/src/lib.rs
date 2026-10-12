//! muse-lean：微型 Lean4 系統（Rust 實作）。
//!
//! 架構：parser（簡化 Lean 語法）→ kernel（dependent type kernel）
//! → tactic（intro/exact/apply/rw/simp/cases/induction…）→ driver（逐條驗證）。

pub mod ast;
pub mod driver;
pub mod kernel;
pub mod parser;
pub mod tactic;
