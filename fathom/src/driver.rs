use codespan_reporting::diagnostic::{Diagnostic, Label, Severity};
use codespan_reporting::files::SimpleFiles;
use codespan_reporting::term::termcolor::{BufferedStandardStream, ColorChoice, WriteColor};
use std::cell::RefCell;
use std::io::Read;
use std::path::Path;

use crate::core::binary;
use crate::core::binary::{BufferError, ReadError};
use crate::source::{ByteRange, FileId, Span};
use crate::surface::{self, elaboration};
use crate::{StringInterner, BUG_REPORT_URL};

#[derive(Debug, Copy, Clone)]
pub enum Status {
    Ok,
    Error,
}

impl Status {
    pub fn exit_code(self) -> i32 {
        match self {
            Status::Ok => 0,
            Status::Error => 1,
        }
    }
}

pub struct Driver<'surface, 'core> {
    files: SimpleFiles<String, String>,
    interner: RefCell<StringInterner>,
    surface_scope: scoped_arena::Scope<'surface>,
    core_scope: scoped_arena::Scope<'core>,

    allow_errors: bool,
    seen_errors: RefCell<bool>,
    codespan_config: codespan_reporting::term::Config,
    diagnostic_writer: RefCell<Box<dyn WriteColor>>,

    emit_width: usize,
    emit_writer: RefCell<Box<dyn WriteColor>>,
}

impl<'surface, 'core> Driver<'surface, 'core> {
    pub fn new() -> Driver<'surface, 'core> {
        Driver {
            interner: RefCell::new(StringInterner::new()),
            surface_scope: scoped_arena::Scope::new(),
            core_scope: scoped_arena::Scope::new(),
            files: SimpleFiles::new(),

            allow_errors: false,
            seen_errors: RefCell::new(false),
            codespan_config: codespan_reporting::term::Config::default(),
            diagnostic_writer: RefCell::new(Box::new(BufferedStandardStream::stderr(
                if atty::is(atty::Stream::Stderr) {
                    ColorChoice::Auto
                } else {
                    ColorChoice::Never
                },
            ))),

            emit_width: usize::MAX,
            emit_writer: RefCell::new(Box::new(BufferedStandardStream::stdout(
                if atty::is(atty::Stream::Stdout) {
                    ColorChoice::Auto
                } else {
                    ColorChoice::Never
                },
            ))),
        }
    }

    /// Setup a global panic hook
    pub fn install_panic_hook(&self) {
        use crate::core::semantics;

        // Use the currently set codespan configuration
        let term_config = self.codespan_config.clone();
        // Fetch the default hook (which prints the panic message and an optional backtrace)
        let default_hook = std::panic::take_hook();

        std::panic::set_hook(Box::new(move |info| {
            let location = info.location();
            let message = if let Some(error) = info.payload().downcast_ref::<semantics::Error>() {
                error.description()
            } else if let Some(message) = info.payload().downcast_ref::<String>() {
                message.as_str()
            } else if let Some(message) = info.payload().downcast_ref::<&str>() {
                message
            } else {
                "unknown panic type"
            };

            let diagnostic = Diagnostic::bug()
                .with_message(format!("compiler panicked at '{}'", message))
                .with_notes(vec![
                    match location {
                        Some(location) => format!("panicked at: {}", location),
                        None => format!("panicked at: unknown location"),
                    },
                    format!("please file a bug report at: {}", BUG_REPORT_URL),
                    // TODO: print rust backtrace
                    // TODO: print fathom backtrace
                ]);

            let mut writer = BufferedStandardStream::stderr(if atty::is(atty::Stream::Stderr) {
                ColorChoice::Auto
            } else {
                ColorChoice::Never
            });
            let dummy_files = SimpleFiles::<String, String>::new();

            default_hook(info);
            eprintln!();
            codespan_reporting::term::emit(&mut writer, &term_config, &dummy_files, &diagnostic)
                .unwrap();
        }));
    }

    /// Set to true if we should attempt to continue after encountering errors
    pub fn set_allow_errors(&mut self, allow_errors: bool) {
        self.allow_errors = allow_errors;
    }

    /// Set the writer to use when rendering diagnostics
    pub fn set_diagnostic_writer(&mut self, stream: impl 'static + WriteColor) {
        self.diagnostic_writer = RefCell::new(Box::new(stream) as Box<dyn WriteColor>);
    }

    /// Set the width to use when emitting data and intermediate languages
    pub fn set_emit_width(&mut self, emit_width: usize) {
        self.emit_width = emit_width;
    }

    /// Set the writer to use when emitting data and intermediate languages
    pub fn set_emit_writer(&mut self, stream: impl 'static + WriteColor) {
        self.emit_writer = RefCell::new(Box::new(stream) as Box<dyn WriteColor>);
    }

    /// Load a source string into the file database.
    pub fn load_source_string(&mut self, name: String, source: String) -> FileId {
        self.files.add(name.to_owned(), source)
    }

    /// Load a source file into the file database using a reader.
    pub fn load_source(&mut self, name: String, mut reader: impl Read) -> Option<FileId> {
        let mut source = String::new();
        match reader.read_to_string(&mut source) {
            Ok(_) => Some(self.load_source_string(name, source)),
            Err(error) => {
                self.emit_read_diagnostic(name, error);
                None
            }
        }
    }

    /// Load a source file into the file database from the given path.
    pub fn load_source_path(&mut self, path: &Path) -> Option<FileId> {
        match std::fs::File::open(path) {
            Ok(file) => self.load_source(path.display().to_string(), file),
            Err(error) => {
                self.emit_read_diagnostic(path.display(), error);
                None
            }
        }
    }

    /// Read all the bytes from a reader into a vector.
    pub fn read_bytes(&mut self, name: String, mut reader: impl Read) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        match reader.read_to_end(&mut bytes) {
            Ok(_) => Some(bytes),
            Err(error) => {
                self.emit_read_diagnostic(name, error);
                None
            }
        }
    }

    /// Read all the bytes in a given file.
    pub fn read_bytes_path(&mut self, path: &Path) -> Option<Vec<u8>> {
        match std::fs::File::open(path) {
            Ok(file) => self.read_bytes(path.display().to_string(), file),
            Err(error) => {
                self.emit_read_diagnostic(path.display(), error);
                None
            }
        }
    }

    pub fn elaborate_and_emit_module(&mut self, file_id: FileId) -> Status {
        let err_scope = scoped_arena::Scope::new();
        let mut context = elaboration::Context::new(&self.interner, &self.core_scope, &err_scope);

        let surface_module = self.parse_module(file_id);
        let module = context.elab_module(&surface_module);

        // Emit errors we might have found during elaboration
        let elab_messages = context.drain_messages();
        self.emit_diagnostics(elab_messages.map(|m| m.to_diagnostic(&self.interner)));

        // Return early if we’ve seen any errors, unless `allow_errors` is enabled
        if *self.seen_errors.borrow() && !self.allow_errors {
            return Status::Error;
        }

        self.surface_scope.reset(); // Reuse the surface scope for distillation
        let context = context.distillation_context(&self.surface_scope);
        let module = context.distill_module(&module);

        self.emit_module(&module);

        Status::Ok
    }

    pub fn elaborate_and_emit_term(&mut self, file_id: FileId) -> Status {
        let err_scope = scoped_arena::Scope::new();
        let mut context = elaboration::Context::new(&self.interner, &self.core_scope, &err_scope);

        // Parse and elaborate the term
        let surface_term = self.parse_term(file_id);
        let (term, r#type) = context.synth(&surface_term);
        let r#type = context.quote_context(&self.core_scope).quote(&r#type);

        // Emit errors we might have found during elaboration
        let elab_messages = context.drain_messages();
        self.emit_diagnostics(elab_messages.map(|m| m.to_diagnostic(&self.interner)));

        // Return early if we’ve seen any errors, unless `allow_errors` is enabled
        if *self.seen_errors.borrow() && !self.allow_errors {
            return Status::Error;
        }

        self.surface_scope.reset(); // Reuse the surface scope for distillation
        let mut context = context.distillation_context(&self.surface_scope);
        let term = context.check(&term);
        let r#type = context.check(&r#type);

        self.emit_term(&surface::Term::Ann((), &term, &r#type));

        Status::Ok
    }

    pub fn normalise_and_emit_term(&mut self, file_id: FileId) -> Status {
        let err_scope = scoped_arena::Scope::new();
        let mut context = elaboration::Context::new(&self.interner, &self.core_scope, &err_scope);

        // Parse and elaborate the term
        let surface_term = self.parse_term(file_id);
        let (term, r#type) = context.synth(&surface_term);

        // Emit errors we might have found during elaboration
        let elab_messages = context.drain_messages();
        self.emit_diagnostics(elab_messages.map(|m| m.to_diagnostic(&self.interner)));

        // Return early if we’ve seen any errors, unless `allow_errors` is enabled
        if *self.seen_errors.borrow() && !self.allow_errors {
            return Status::Error;
        }

        let term = context.eval_context().normalise(&self.core_scope, &term);
        let r#type = context.quote_context(&self.core_scope).quote(&r#type);

        self.surface_scope.reset(); // Reuse the surface scope for distillation
        let mut context = context.distillation_context(&self.surface_scope);
        let term = context.check(&term);
        let r#type = context.check(&r#type);

        self.emit_term(&surface::Term::Ann((), &term, &r#type));

        Status::Ok
    }

    pub fn read_and_emit_format<'data>(
        &mut self,
        module_file_id: Option<FileId>,
        format_file_id: FileId,
        buffer_data: &[u8],
    ) -> Status {
        use itertools::Itertools;
        use std::sync::Arc;

        use crate::core::semantics::Value;
        use crate::core::Prim;

        let err_scope = scoped_arena::Scope::new();
        let mut context = elaboration::Context::new(&self.interner, &self.core_scope, &err_scope);

        // Parse and elaborate the supplied module
        if let Some(file_id) = module_file_id {
            let surface_module = self.parse_module(file_id);
            context.elab_module(&surface_module);
        }

        // Parse and elaborate the supplied format with the items from the
        // supplied in the module in scope. This is still a bit of a hack, and
        // will need to be revisited if we need to support multiple modules, but
        // it works for now!
        let surface_format = self.parse_term(format_file_id);
        let format_term = context.check(
            &surface_format,
            &Arc::new(Value::prim(Prim::FormatType, [])),
        );

        // Emit errors we might have found during elaboration
        let elab_messages = context.drain_messages();
        self.emit_diagnostics(elab_messages.map(|m| m.to_diagnostic(&self.interner)));

        // Return early if we’ve seen any errors, unless `allow_errors` is enabled
        if *self.seen_errors.borrow() && !self.allow_errors {
            return Status::Error;
        }

        let format = context.eval_context().eval(&format_term);
        let buffer = binary::Buffer::from(buffer_data);
        let refs = match context.binary_context(buffer).read_entrypoint(format) {
            Ok(refs) => refs,
            Err(err) => {
                self.emit_diagnostic(Diagnostic::from(err));
                return Status::Error;
            }
        };

        // Render the data we have read
        for (pos, parsed_refs) in refs.into_iter().sorted_by_key(|(pos, _)| *pos) {
            self.surface_scope.reset(); // Reuse the surface scope for distillation

            let exprs = parsed_refs.iter().map(|parsed_ref| {
                let core_scope = &self.core_scope;
                let surface_scope = &self.surface_scope;
                let expr = context.quote_context(core_scope).quote(&parsed_ref.expr);
                context.distillation_context(surface_scope).check(&expr)
            });

            self.emit_ref(pos, exprs.collect());
        }

        Status::Ok
    }

    fn parse_module(&'surface self, file_id: FileId) -> surface::Module<'surface, ByteRange> {
        let source = self.files.get(file_id).unwrap().source();
        let (module, messages) =
            surface::Module::parse(&self.interner, &self.surface_scope, file_id, source);
        self.emit_diagnostics(messages.into_iter().map(|m| m.to_diagnostic()));

        module
    }

    fn parse_term(&'surface self, file_id: FileId) -> surface::Term<'surface, ByteRange> {
        let source = self.files.get(file_id).unwrap().source();
        let (term, messages) =
            surface::Term::parse(&self.interner, &self.surface_scope, file_id, source);
        self.emit_diagnostics(messages.into_iter().map(move |m| m.to_diagnostic()));

        term
    }

    fn emit_module(&self, module: &surface::Module<'_, ()>) {
        let context = surface::pretty::Context::new(&self.interner, &self.surface_scope);
        self.emit_doc(context.module(module).into_doc());
    }

    fn emit_term(&self, term: &surface::Term<'_, ()>) {
        let context = surface::pretty::Context::new(&self.interner, &self.surface_scope);
        self.emit_doc(context.term(term).into_doc());
    }

    fn emit_ref(&self, pos: usize, exprs: Vec<surface::Term<'_, ()>>) {
        use pretty::DocAllocator;

        let context = surface::pretty::Context::new(&self.interner, &self.surface_scope);
        let pos = pos.to_string();
        let doc = context
            .concat([
                context.text(&pos),
                context.space(),
                context.text("="),
                context.space(),
                context.sequence(
                    context.text("["),
                    exprs.iter().map(|expr| context.term(&expr)),
                    context.text(","),
                    context.text("]"),
                ),
            ])
            .into_doc();

        self.emit_doc(doc);
    }

    fn emit_doc(&self, doc: pretty::RefDoc) {
        let mut emit_writer = self.emit_writer.borrow_mut();
        writeln!(emit_writer, "{}", doc.pretty(self.emit_width)).unwrap();
        emit_writer.flush().unwrap();
    }

    fn emit_diagnostic(&self, diagnostic: Diagnostic<FileId>) {
        let mut writer = self.diagnostic_writer.borrow_mut();
        let config = &self.codespan_config;

        codespan_reporting::term::emit(&mut *writer, config, &self.files, &diagnostic).unwrap();
        writer.flush().unwrap();

        if diagnostic.severity >= Severity::Error {
            *self.seen_errors.borrow_mut() = true;
        }
    }

    fn emit_diagnostics(&self, diagnostics: impl Iterator<Item = Diagnostic<FileId>>) {
        for diagnostic in diagnostics {
            self.emit_diagnostic(diagnostic);
        }
    }

    fn emit_read_diagnostic(&self, name: impl std::fmt::Display, error: std::io::Error) {
        let diagnostic =
            Diagnostic::error().with_message(format!("couldn't read `{}`: {}", name, error));
        self.emit_diagnostic(diagnostic);
    }
}

impl From<ReadError> for Diagnostic<usize> {
    fn from(err: ReadError) -> Diagnostic<usize> {
        let primary_label = |span: &Span| match span {
            Span::Range(range) => Some(Label::primary(range.file_id(), *range)),
            Span::Empty => None,
        };

        match err {
            ReadError::ReadFailFormat => Diagnostic::error()
                .with_message(err.to_string())
                .with_notes(vec![format!(
                    "A fail format was encountered when reading this file."
                )]),
            ReadError::CondFailure(span) => Diagnostic::error()
                .with_message(err.to_string())
                .with_labels(
                    IntoIterator::into_iter([primary_label(&span)])
                        .into_iter()
                        .flatten()
                        .collect(),
                )
                .with_notes(vec![format!(
                    "The predicate on a conditional format did not succeed."
                )]),
            ReadError::UnwrappedNone(_) => Diagnostic::error()
                .with_message(err.to_string())
                .with_notes(vec![format!("option_unwrap was called on a none value.")]),
            ReadError::BufferError(BufferError::UnexpectedEndOfBuffer) => Diagnostic::error()
                .with_message(err.to_string())
                .with_notes(vec![format!(
                    "The end of the buffer was reached before all data could be read."
                )]),
            ReadError::BufferError(BufferError::SetOffsetBeforeStartOfBuffer { offset }) => {
                Diagnostic::error()
                    .with_message(err.to_string())
                    .with_notes(vec![format!(
                        "The offset {} is before the start of the buffer.",
                        offset
                    )])
            }
            ReadError::BufferError(BufferError::SetOffsetAfterEndOfBuffer {
                offset: Some(offset),
            }) => Diagnostic::error()
                .with_message(err.to_string())
                .with_notes(vec![format!(
                    "The offset {} is beyond the end of the buffer.",
                    offset
                )]),
            ReadError::BufferError(BufferError::SetOffsetAfterEndOfBuffer { offset: None }) => {
                Diagnostic::error()
                    .with_message(err.to_string())
                    .with_notes(vec![format!(
                        "The offset is beyond the end of the buffer (overflow).",
                    )])
            }
            ReadError::InvalidFormat(span) | ReadError::InvalidValue(span) => Diagnostic::bug()
                .with_message(format!("unexpected error '{}'", err))
                .with_labels(
                    IntoIterator::into_iter([primary_label(&span)])
                        .into_iter()
                        .flatten()
                        .collect(),
                )
                .with_notes(vec![format!(
                    "please file a bug report at: {}",
                    BUG_REPORT_URL
                )]),
            ReadError::UnknownItem | ReadError::BufferError(BufferError::PositionOverflow) => {
                Diagnostic::bug()
                    .with_message(format!("unexpected error '{}'", err))
                    .with_notes(vec![format!(
                        "please file a bug report at: {}",
                        BUG_REPORT_URL
                    )])
            }
        }
    }
}
