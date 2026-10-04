//! AST → bytecode.

use crate::bytecode::CompiledProgram;
use surf_syntax::ast::Program;
use surf_syntax::{Diagnostic, Diagnostics, Span};

/// A compile-time error (undefined break target, assignment to a non-place,
/// property line after a statement, …).
#[derive(Debug, Clone, PartialEq)]
pub struct CompileError {
    /// Message.
    pub message: String,
    /// Where.
    pub span: Span,
}

impl CompileError {
    /// Convert to renderable diagnostics.
    pub fn into_diagnostics(self, name: &str) -> Diagnostics {
        Diagnostics::single(name, Diagnostic::error(self.message, self.span))
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CompileError {}

/// Compile a parsed program. Implemented in task 6.
pub fn compile(name: &str, program: &Program) -> Result<CompiledProgram, CompileError> {
    let _ = name;
    Err(CompileError {
        message: "compiler not implemented yet".into(),
        span: program.span,
    })
}
