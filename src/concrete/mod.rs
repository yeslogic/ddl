//! The concrete syntax for the data description language.

use codespan::{ByteIndex, ByteOffset, FileId, Span};
use std::rc::Rc;

/// A module of items.
#[derive(Debug, Clone)]
pub struct Module {
    /// The file in which this module was defined.
    pub file_id: FileId,
    /// The items in this module.
    pub items: Vec<Item>,
}

/// Items in a module.
#[derive(Debug, Clone)]
pub enum Item {
    /// Struct definitions.
    ///
    /// ```text
    /// struct <name> {}
    /// ```
    Struct {
        /// The full span of this definition.
        span: Span,
        /// Doc comment.
        doc: Rc<str>,
        /// Name of this definition.
        name: SpannedString,
    },
}

/// A string that is located in a source file.
#[derive(Debug, Clone, derive_more::Display)]
#[display(fmt = "{}", inner)]
pub struct SpannedString {
    pub start: ByteIndex,
    pub inner: String,
}

impl SpannedString {
    pub fn new(start: impl Into<ByteIndex>, inner: impl Into<String>) -> SpannedString {
        SpannedString {
            start: start.into(),
            inner: inner.into(),
        }
    }

    pub fn span(&self) -> Span {
        Span::new(
            self.start,
            self.start + ByteOffset::from_str_len(&self.inner),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.inner
    }
}
