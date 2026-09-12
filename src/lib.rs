//! folio — ターミナルで文書を読み、そのまま編集に入るための道具。
//!
//! 中核（`document` / `render`）はターミナルに触らない。ターミナル制御は
//! `viewer`に閉じ込める。将来ファイラー本体を自作した時に、プレビュー欄が
//! 同じ中核をそのまま呼べるようにするため（REQUIREMENTS.md「全体に効く方針」）。

pub mod document;
pub mod render;
pub mod viewer;
