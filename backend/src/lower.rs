//! Backend lowering methods, split from `lib.rs` (Phase 1) along its seams.
//! Every submodule adds `impl Lowerer<'_>` blocks to the type defined in the
//! crate root; nothing here changes behaviour.
mod aggregate;
mod calls;
mod expr;
mod finish;
mod forms;
mod stmt;
mod sugar;
