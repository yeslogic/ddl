//! Elaboration of the surface language into the core language.
//!
//! This module is where user-facing type checking happens, along with
//! translating the convenient surface language into a simpler, more explicit
//! core language.
//!
//! The algorithm is structured _bidirectionally_, ie. divided into _checking_
//! and _synthesis_ modes. By supplying type annotations as early as possible
//! using the checking mode, we can improve the locality of type errors, and
//! provide enough _control_ to the algorithm to allow for elaboration even in
//! the presence of ‘fancy’ types.
//!
//! For places where bidirectional typing is not enough, _unification_ is used
//! in an attempt to infer unknown terms and types based on how they are used.
//!
//! ## Resources
//!
//! - [Bidirectional Typing Rules: A Tutorial](https://davidchristiansen.dk/tutorials/bidirectional.pdf)
//! - [Bidirectional Types Checking – Compose NYC 2019](https://www.youtube.com/watch?v=utyBNDj7s2w)
//! - [Lecture Notes on Bidirectional Type Checking](https://www.cs.cmu.edu/~fp/courses/15312-f04/handouts/15-bidirectional.pdf)
//! - [elaboration-zoo](https://github.com/AndrasKovacs/elaboration-zoo/)

use std::str::FromStr;
use std::sync::Arc;

use scoped_arena::Scope;

use crate::alloc::SliceVec;
use crate::core::semantics::{self, ArcValue, Head, Telescope, Value};
use crate::core::{self, prim, Const, Plicity, Prim, UIntStyle};
use crate::env::{self, EnvLen, Level, SharedEnv, UniqueEnv};
use crate::files::FileId;
use crate::source::{BytePos, ByteRange, FileRange, Span, Spanned};
use crate::surface::elaboration::reporting::Message;
use crate::surface::{
    distillation, pretty, BinOp, FormatField, Item, LetDef, Module, Param, Pattern, Term,
};
use crate::symbol::Symbol;

mod order;
mod reporting;
mod unification;

/// Top-level item environment.
pub struct ItemEnv<'arena> {
    /// Names of items.
    names: UniqueEnv<Symbol>,
    /// Types of items.
    types: UniqueEnv<ArcValue<'arena>>,
    /// Expressions of items.
    exprs: UniqueEnv<ArcValue<'arena>>,
}

impl<'arena> ItemEnv<'arena> {
    /// Construct a new, empty environment.
    pub fn new() -> ItemEnv<'arena> {
        ItemEnv {
            names: UniqueEnv::new(),
            types: UniqueEnv::new(),
            exprs: UniqueEnv::new(),
        }
    }

    fn push_definition(&mut self, name: Symbol, r#type: ArcValue<'arena>, expr: ArcValue<'arena>) {
        self.names.push(name);
        self.types.push(r#type);
        self.exprs.push(expr);
    }

    fn reserve(&mut self, additional: usize) {
        self.names.reserve(additional);
        self.types.reserve(additional);
        self.exprs.reserve(additional);
    }
}

/// Local variable environment.
///
/// This is used for keeping track of [local variables] that are bound by the
/// program, for example by function parameters, let bindings, or pattern
/// matching.
///
/// This environment behaves as a stack.
/// - As scopes are entered, it is important to remember to call either
///   [`LocalEnv::push_def`] or [`LocalEnv::push_param`].
/// - On scope exit, it is important to remember to call [`LocalEnv::pop`].
/// - Multiple bindings can be removed at once with [`LocalEnv::truncate`].
///
/// [local variables]: core::Term::LocalVar
struct LocalEnv<'arena> {
    /// Names of local variables.
    names: UniqueEnv<Option<Symbol>>,
    /// Types of local variables.
    types: UniqueEnv<ArcValue<'arena>>,
    /// Information about the local binders. Used when inserting new
    /// metavariables during [evaluation][semantics::EvalEnv::eval].
    infos: UniqueEnv<core::LocalInfo>,
    /// Expressions that will be substituted for local variables during
    /// [evaluation][semantics::EvalEnv::eval].
    exprs: SharedEnv<ArcValue<'arena>>,
}

impl<'arena> LocalEnv<'arena> {
    /// Construct a new, empty environment.
    fn new() -> LocalEnv<'arena> {
        LocalEnv {
            names: UniqueEnv::new(),
            types: UniqueEnv::new(),
            infos: UniqueEnv::new(),
            exprs: SharedEnv::new(),
        }
    }

    /// Get the length of the local environment.
    fn len(&self) -> EnvLen {
        self.names.len()
    }

    fn reserve(&mut self, additional: usize) {
        self.names.reserve(additional);
        self.types.reserve(additional);
        self.infos.reserve(additional);
        self.exprs.reserve(additional);
    }

    /// Push a local definition onto the context.
    fn push_def(&mut self, name: Option<Symbol>, expr: ArcValue<'arena>, r#type: ArcValue<'arena>) {
        self.names.push(name);
        self.types.push(r#type);
        self.infos.push(core::LocalInfo::Def);
        self.exprs.push(expr);
    }

    /// Push a local parameter onto the context.
    fn push_param(&mut self, name: Option<Symbol>, r#type: ArcValue<'arena>) -> ArcValue<'arena> {
        // An expression that refers to itself once it is pushed onto the local
        // expression environment.
        let expr = Spanned::empty(Arc::new(Value::local_var(self.exprs.len().next_level())));

        self.names.push(name);
        self.types.push(r#type);
        self.infos.push(core::LocalInfo::Param);
        self.exprs.push(expr.clone());

        expr
    }

    /// Pop a local binder off the context.
    fn pop(&mut self) {
        self.names.pop();
        self.types.pop();
        self.infos.pop();
        self.exprs.pop();
    }

    /// Truncate the local environment.
    fn truncate(&mut self, len: EnvLen) {
        self.names.truncate(len);
        self.types.truncate(len);
        self.infos.truncate(len);
        self.exprs.truncate(len);
    }
}

/// The reason why a metavariable was inserted.
#[derive(Debug, Copy, Clone)]
pub enum MetaSource {
    ImplicitArg(FileRange, Option<Symbol>),
    /// The type of a hole.
    HoleType(FileRange, Symbol),
    /// The expression of a hole.
    HoleExpr(FileRange, Symbol),
    /// The type of a placeholder
    PlaceholderType(FileRange),
    /// The expression of a placeholder
    PlaceholderExpr(FileRange),
    /// The type of a placeholder pattern.
    PlaceholderPatternType(FileRange),
    /// The type of a named pattern.
    NamedPatternType(FileRange, Symbol),
    /// The overall type of a match expression
    MatchExprType(FileRange),
    /// The type of a reported error.
    ReportedErrorType(FileRange),
}

impl MetaSource {
    pub fn range(&self) -> FileRange {
        match self {
            MetaSource::ImplicitArg(range, _)
            | MetaSource::HoleType(range, _)
            | MetaSource::HoleExpr(range, _)
            | MetaSource::PlaceholderType(range)
            | MetaSource::PlaceholderExpr(range)
            | MetaSource::PlaceholderPatternType(range)
            | MetaSource::NamedPatternType(range, _)
            | MetaSource::MatchExprType(range)
            | MetaSource::ReportedErrorType(range) => *range,
        }
    }
}

/// Metavariable environment.
///
/// This is used for keeping track of the state of [metavariables] whose
/// definitions are intended to be found through the use of [unification].
///
/// [metavariables]: core::Term::MetaVar
struct MetaEnv<'arena> {
    /// The source of inserted metavariables, used when reporting [unsolved
    /// metavariables][Message::UnsolvedMetaVar].
    sources: UniqueEnv<MetaSource>,
    /// Types of metavariables.
    types: UniqueEnv</* TODO: lazy value */ ArcValue<'arena>>,
    /// Expressions that will be substituted for metavariables during
    /// [evaluation][semantics::EvalEnv::eval].
    ///
    /// These will be set to [`None`] when a metavariable is first
    /// [inserted][Context::push_unsolved_term], then will be set to [`Some`]
    /// if a solution is found during [`unification`].
    exprs: UniqueEnv<Option<ArcValue<'arena>>>,
}

impl<'arena> MetaEnv<'arena> {
    /// Construct a new, empty environment.
    fn new() -> MetaEnv<'arena> {
        MetaEnv {
            sources: UniqueEnv::new(),
            types: UniqueEnv::new(),
            exprs: UniqueEnv::new(),
        }
    }

    /// Push an unsolved metavariable onto the context.
    fn push(&mut self, source: MetaSource, r#type: ArcValue<'arena>) -> Level {
        // TODO: check that hole name is not already in use
        let var = self.exprs.len().next_level();

        self.sources.push(source);
        self.types.push(r#type);
        self.exprs.push(None);

        var
    }
}

/// Elaboration context.
pub struct Context<'arena> {
    file_id: FileId,
    /// Scoped arena for storing elaborated terms.
    //
    // TODO: Make this local to the elaboration context, and reallocate
    //       elaborated terms to an external `Scope` during zonking, resetting
    //       this scope on completion.
    scope: &'arena Scope<'arena>,

    // Commonly used values, cached to increase sharing.
    universe: ArcValue<'static>,
    format_type: ArcValue<'static>,
    bool_type: ArcValue<'static>,

    /// Primitive environment.
    prim_env: prim::Env<'arena>,
    /// Item environment.
    item_env: ItemEnv<'arena>,
    /// Meta environment.
    meta_env: MetaEnv<'arena>,
    /// Local environment.
    local_env: LocalEnv<'arena>,
    /// A partial renaming to be used during [`unification`].
    renaming: unification::PartialRenaming,
    /// Diagnostic messages encountered during elaboration.
    messages: Vec<Message>,
}

fn suggest_name(name: Symbol, candidates: impl Iterator<Item = Symbol>) -> Option<Symbol> {
    let name = name.resolve();
    candidates.min_by_key(|candidate| {
        let candidate = candidate.resolve();
        levenshtein::levenshtein(name, candidate)
    })
}

impl<'arena> Context<'arena> {
    /// Construct a new elaboration context, backed by the supplied arena.
    pub fn new(
        file_id: FileId,
        scope: &'arena Scope<'arena>,
        item_env: ItemEnv<'arena>,
    ) -> Context<'arena> {
        Context {
            file_id,
            scope,

            universe: Spanned::empty(Arc::new(Value::Universe)),
            format_type: Spanned::empty(Arc::new(Value::prim(Prim::FormatType, []))),
            bool_type: Spanned::empty(Arc::new(Value::prim(Prim::BoolType, []))),

            prim_env: prim::Env::default(scope),
            item_env,
            meta_env: MetaEnv::new(),
            local_env: LocalEnv::new(),
            renaming: unification::PartialRenaming::new(),
            messages: Vec::new(),
        }
    }

    pub fn finish(self) -> ItemEnv<'arena> {
        self.item_env
    }

    fn file_range(&self, byte_range: ByteRange) -> FileRange {
        FileRange::new(self.file_id, byte_range)
    }

    /// Lookup an item name in the context.
    fn get_item_name(&self, name: Symbol) -> Option<(Level, &ArcValue<'arena>)> {
        let item_var = self.item_env.names.elem_level(&name)?;
        let item_type = self.item_env.types.get_level(item_var)?;

        Some((item_var, item_type))
    }

    /// Lookup a local name in the context.
    fn get_local_name(&self, name: Symbol) -> Option<(env::Index, &ArcValue<'arena>)> {
        let local_var = self.local_env.names.elem_index(&Some(name))?;
        let local_type = self.local_env.types.get_index(local_var)?;

        Some((local_var, local_type))
    }

    /// Run `f`, potentially modifying the local environment, then restore the
    /// local environment to its previous state.
    fn with_scope<T>(&mut self, mut f: impl FnMut(&mut Self) -> T) -> T {
        let initial_len = self.local_env.len();
        let result = f(self);
        self.local_env.truncate(initial_len);
        result
    }

    fn with_def<T>(
        &mut self,
        name: impl Into<Option<Symbol>>,
        expr: ArcValue<'arena>,
        r#type: ArcValue<'arena>,
        mut f: impl FnMut(&mut Self) -> T,
    ) -> T {
        self.local_env.push_def(name.into(), expr, r#type);
        let result = f(self);
        self.local_env.pop();
        result
    }

    fn with_param<T>(
        &mut self,
        name: impl Into<Option<Symbol>>,
        r#type: ArcValue<'arena>,
        mut f: impl FnMut(&mut Self) -> T,
    ) -> T {
        self.local_env.push_param(name.into(), r#type);
        let result = f(self);
        self.local_env.pop();
        result
    }

    /// Push an unsolved term onto the context, to be updated later during
    /// unification.
    fn push_unsolved_term(
        &mut self,
        source: MetaSource,
        r#type: ArcValue<'arena>,
    ) -> core::Term<'arena> {
        core::Term::InsertedMeta(
            source.range().into(),
            self.meta_env.push(source, r#type),
            (self.scope).to_scope_from_iter(self.local_env.infos.iter().copied()),
        )
    }

    /// Push an unsolved type onto the context, to be updated later during
    /// unification.
    fn push_unsolved_type(&mut self, source: MetaSource) -> ArcValue<'arena> {
        let r#type = self.push_unsolved_term(source, self.universe.clone());
        self.eval_env().eval(&r#type)
    }

    fn push_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn handle_messages(&mut self, on_message: &mut dyn FnMut(Message)) {
        for message in self.messages.drain(..) {
            on_message(message);
        }

        let meta_env = &self.meta_env;
        for (expr, source) in Iterator::zip(meta_env.exprs.iter(), meta_env.sources.iter()) {
            match (expr, *source) {
                // Avoid producing messages for some unsolved metavariable sources:
                // Should have an unsolved hole expression
                (None, MetaSource::HoleType(_, _)) => {}
                // Should have an unsolved placeholder
                (None, MetaSource::PlaceholderType(_)) => {}
                // Should already have an error
                (None, MetaSource::ReportedErrorType(_)) => {}

                // For other sources, report an unsolved problem message
                (None, source) => on_message(Message::UnsolvedMetaVar { source }),
                // Yield messages of solved named holes
                (Some(expr), MetaSource::HoleExpr(range, name)) => {
                    let expr = self.pretty_value(expr);
                    on_message(Message::HoleSolution { range, name, expr });
                }
                // Ignore solutions of anything else
                (Some(_), _) => {}
            }
        }
    }

    pub fn builder(&self) -> core::Builder<'arena> {
        core::Builder::new(self.scope)
    }

    pub fn eval_env(&mut self) -> semantics::EvalEnv<'arena, '_> {
        semantics::ElimEnv::new(&self.item_env.exprs, &self.meta_env.exprs)
            .eval_env(&mut self.local_env.exprs)
    }

    pub fn elim_env(&self) -> semantics::ElimEnv<'arena, '_> {
        semantics::ElimEnv::new(&self.item_env.exprs, &self.meta_env.exprs)
    }

    pub fn quote_env(&self) -> semantics::QuoteEnv<'arena, '_> {
        semantics::QuoteEnv::new(self.elim_env(), self.local_env.len())
    }

    fn unification_context(&mut self) -> unification::Context<'arena, '_> {
        unification::Context::new(
            self.scope,
            &mut self.renaming,
            &self.item_env.exprs,
            self.local_env.len(),
            &mut self.meta_env.exprs,
        )
    }

    pub fn distillation_context<'out_arena>(
        &self,
        scope: &'out_arena Scope<'out_arena>,
    ) -> distillation::Context<'out_arena, '_> {
        distillation::Context::new(
            scope,
            &self.item_env.names,
            self.local_env.names.clone(),
            &self.meta_env.sources,
        )
    }

    fn pretty_value(&self, value: &ArcValue<'_>) -> String {
        let term = self.quote_env().unfolding_metas().quote(self.scope, value);
        let surface_term = self.distillation_context(self.scope).check(&term);

        pretty::Context::new(self.scope)
            .term(&surface_term)
            .pretty(usize::MAX)
            .to_string()
    }

    /// Reports an error if there are duplicate fields found, returning a slice
    /// of the labels unique labels and an iterator over the unique fields.
    fn report_duplicate_labels<'fields, F>(
        &mut self,
        range: ByteRange,
        fields: &'fields [F],
        get_label: fn(&F) -> (ByteRange, Symbol),
    ) -> (&'arena [Symbol], impl Iterator<Item = &'fields F>) {
        let mut labels = SliceVec::new(self.scope, fields.len());
        // Will only allocate when duplicates are encountered
        let mut duplicate_indices = Vec::new();
        let mut duplicate_labels = Vec::new();

        for (index, field) in fields.iter().enumerate() {
            let (range, label) = get_label(field);
            if labels.contains(&label) {
                duplicate_indices.push(index);
                duplicate_labels.push((self.file_range(range), label));
            } else {
                labels.push(label)
            }
        }

        if !duplicate_labels.is_empty() {
            self.push_message(Message::DuplicateFieldLabels {
                range: self.file_range(range),
                labels: duplicate_labels,
            });
        }

        let filtered_fields = (fields.iter().enumerate()).filter_map(move |(index, field)| {
            (!duplicate_indices.contains(&index)).then_some(field)
        });

        (labels.into(), filtered_fields)
    }

    /// Parse a source string into number, assuming an ASCII encoding.
    fn parse_ascii<T>(
        &mut self,
        range: ByteRange,
        symbol: Symbol,
        make: fn(T, UIntStyle) -> Const,
    ) -> Option<Const>
    where
        T: From<u8> + std::ops::Shl<Output = T> + std::ops::BitOr<Output = T>,
    {
        // TODO: Parse escape codes
        // TODO: Alternate byte orders
        // TODO: Non-ASCII encodings

        let source = symbol.resolve();
        let mut num = Some(T::from(0));
        let mut count: u8 = 0;

        for (offset, ch) in source.char_indices() {
            if !ch.is_ascii() {
                let ch_start = range.start() + 1 + offset as BytePos;
                let ch_end = ch_start + ch.len_utf8() as BytePos;

                self.push_message(Message::NonAsciiStringLiteral {
                    invalid_range: self.file_range(ByteRange::new(ch_start, ch_end)),
                });
                num = None;
            }

            num = num.filter(|_| usize::from(count) < std::mem::size_of::<T>());
            num = num.map(|num| {
                // Yikes this is a tad ugly. Setting the bytes in reverse order...
                let offset = 8 * (std::mem::size_of::<T>() as u8 - (count + 1));
                num | (T::from(ch as u8) << T::from(offset))
            });
            count += 1;
        }

        if count as usize != std::mem::size_of::<T>() {
            self.push_message(Message::MismatchedStringLiteralByteLength {
                range: self.file_range(range),
                expected_len: std::mem::size_of::<T>(),
                found_len: count as usize,
            });
            num = None;
        }

        num.map(|num| make(num, UIntStyle::Ascii))
    }

    /// Parse a source string into a number.
    fn parse_number<T: FromStr>(
        &mut self,
        range: ByteRange,
        symbol: Symbol,
        make: fn(T) -> Const,
    ) -> Option<Const>
    where
        T::Err: std::fmt::Display,
    {
        // TODO: Custom parsing and improved errors
        match symbol.resolve().parse() {
            Ok(data) => Some(make(data)),
            Err(error) => {
                let message = error.to_string();
                self.push_message(Message::InvalidNumericLiteral {
                    range: self.file_range(range),
                    message,
                });
                None
            }
        }
    }

    /// Parse a source string into a number.
    fn parse_number_radix<T: FromStrRadix>(
        &mut self,
        range: ByteRange,
        symbol: Symbol,
        make: fn(T, UIntStyle) -> Const,
    ) -> Option<Const> {
        // TODO: Custom parsing and improved errors
        let s = symbol.resolve();
        let (s, radix, style) = if let Some(s) = s.strip_prefix("0x") {
            (s, 16, UIntStyle::Hexadecimal)
        } else if let Some(s) = s.strip_prefix("0b") {
            (s, 2, UIntStyle::Binary)
        } else {
            (s, 10, UIntStyle::Decimal)
        };
        match T::from_str_radix(s, radix) {
            Ok(data) => Some(make(data, style)),
            Err(error) => {
                let message = error.to_string();
                self.push_message(Message::InvalidNumericLiteral {
                    range: self.file_range(range),
                    message,
                });
                None
            }
        }
    }

    /// Coerce an expression from one type to another type. This will trigger
    /// unification, recording a unification error on failure.
    fn coerce(
        &mut self,
        surface_range: ByteRange, /* TODO: could be removed if we never encounter empty spans in
                                   * the core term */
        expr: core::Term<'arena>,
        from: &ArcValue<'arena>,
        to: &ArcValue<'arena>,
    ) -> core::Term<'arena> {
        let span = expr.span();
        let from = self.elim_env().force(from);
        let to = self.elim_env().force(to);

        match (from.as_ref(), to.as_ref()) {
            // Coerce format descriptions to their representation types by
            // applying `Repr`.
            (Value::Stuck(Head::Prim(Prim::FormatType), elims), Value::Universe)
                if elims.is_empty() =>
            {
                self.builder().fun_app(
                    span,
                    Plicity::Explicit,
                    core::Term::Prim(span, core::Prim::FormatRepr),
                    expr,
                )
            }

            // Otherwise, unify the types
            (_, _) => match self.unification_context().unify(&from, &to) {
                Ok(()) => expr,
                Err(error) => {
                    let range = match span {
                        Span::Range(range) => range,
                        Span::Empty => {
                            let range = self.file_range(surface_range);
                            self.push_message(Message::MissingSpan { range });
                            range
                        }
                    };

                    self.push_message(Message::FailedToUnify {
                        range,
                        found: self.pretty_value(&from),
                        expected: self.pretty_value(&to),
                        error,
                    });
                    core::Term::error(span)
                }
            },
        }
    }

    /// Elaborate a module.
    pub fn elab_module<'out_arena>(
        &mut self,
        scope: &'out_arena Scope<'out_arena>,
        surface_module: &Module<'_, ByteRange>,
        on_message: &mut dyn FnMut(Message),
    ) -> core::Module<'out_arena> {
        let elab_order = order::elaboration_order(self, surface_module);
        let mut items = Vec::with_capacity(surface_module.items.len());
        self.item_env.reserve(surface_module.items.len());

        for item in elab_order.iter().copied().map(|i| &surface_module.items[i]) {
            match item {
                Item::Def(item) => {
                    let (expr, r#type) = self.synth_fun_lit(
                        item.range,
                        item.params,
                        &item.expr,
                        item.r#type.as_ref(),
                    );
                    let expr_value = self.eval_env().eval(&expr);
                    let type_value = self.eval_env().eval(&r#type);

                    self.item_env
                        .push_definition(item.label.1, type_value, expr_value);

                    items.push(core::Item::Def {
                        label: item.label.1,
                        r#type,
                        expr,
                    });
                }
                Item::ReportedError(_) => {}
            }
        }

        // Unfold all unification solutions
        let items = scope.to_scope_from_iter(items.into_iter().map(|item| match item {
            core::Item::Def {
                label,
                r#type,
                expr,
            } => {
                // TODO: Unfold unsolved metas to reported errors
                let r#type = self.eval_env().unfold_metas(scope, &r#type);
                let expr = self.eval_env().unfold_metas(scope, &expr);

                core::Item::Def {
                    label,
                    r#type,
                    expr,
                }
            }
        }));

        self.handle_messages(on_message);

        // TODO: Clear environments
        // TODO: Reset scopes

        core::Module { items }
    }

    /// Elaborate a term, returning its synthesized type.
    pub fn elab_term<'out_arena>(
        &mut self,
        scope: &'out_arena Scope<'out_arena>,
        surface_term: &Term<'_, ByteRange>,
        on_message: &mut dyn FnMut(Message),
    ) -> (core::Term<'out_arena>, core::Term<'out_arena>) {
        let (term, r#type) = self.synth(surface_term);
        let term = self.eval_env().unfold_metas(scope, &term);
        let r#type = self.quote_env().unfolding_metas().quote(scope, &r#type);

        self.handle_messages(on_message);

        // TODO: Clear environments
        // TODO: Reset scopes

        (term, r#type)
    }

    /// Elaborate a term, expecting it to be a format.
    pub fn elab_format<'out_arena>(
        &mut self,
        scope: &'out_arena Scope<'out_arena>,
        surface_term: &Term<'_, ByteRange>,
        on_message: &mut dyn FnMut(Message),
    ) -> core::Term<'out_arena> {
        let term = self.check(surface_term, &self.format_type.clone());
        let term = self.eval_env().unfold_metas(scope, &term); // TODO: fuse with above?

        self.handle_messages(on_message);

        // TODO: Clear environments
        // TODO: Reset scopes

        term
    }

    /// Check that a pattern matches an expected type.
    fn check_pattern(
        &mut self,
        pattern: &Pattern<ByteRange>,
        expected_type: &ArcValue<'arena>,
    ) -> CheckedPattern {
        let file_range = self.file_range(pattern.range());
        match pattern {
            Pattern::Name(_, name) => CheckedPattern::Binder(file_range, *name),
            Pattern::Placeholder(_) => CheckedPattern::Placeholder(file_range),
            Pattern::StringLiteral(range, lit) => {
                let constant = match expected_type.match_prim_spine() {
                    Some((Prim::U8Type, [])) => self.parse_ascii(*range, *lit, Const::U8),
                    Some((Prim::U16Type, [])) => self.parse_ascii(*range, *lit, Const::U16),
                    Some((Prim::U32Type, [])) => self.parse_ascii(*range, *lit, Const::U32),
                    Some((Prim::U64Type, [])) => self.parse_ascii(*range, *lit, Const::U64),
                    // Some((Prim::Array8Type, [len, _])) => todo!(),
                    // Some((Prim::Array16Type, [len, _])) => todo!(),
                    // Some((Prim::Array32Type, [len, _])) => todo!(),
                    // Some((Prim::Array64Type, [len, _])) => todo!(),
                    Some((Prim::ReportedError, _)) => None,
                    _ => {
                        self.push_message(Message::StringLiteralNotSupported {
                            range: file_range,
                            expected_type: self.pretty_value(expected_type),
                        });
                        None
                    }
                };

                match constant {
                    Some(constant) => CheckedPattern::ConstLit(file_range, constant),
                    None => CheckedPattern::ReportedError(file_range),
                }
            }
            Pattern::NumberLiteral(range, lit) => {
                let constant = match expected_type.match_prim_spine() {
                    Some((Prim::U8Type, [])) => self.parse_number_radix(*range, *lit, Const::U8),
                    Some((Prim::U16Type, [])) => self.parse_number_radix(*range, *lit, Const::U16),
                    Some((Prim::U32Type, [])) => self.parse_number_radix(*range, *lit, Const::U32),
                    Some((Prim::U64Type, [])) => self.parse_number_radix(*range, *lit, Const::U64),
                    Some((Prim::S8Type, [])) => self.parse_number(*range, *lit, Const::S8),
                    Some((Prim::S16Type, [])) => self.parse_number(*range, *lit, Const::S16),
                    Some((Prim::S32Type, [])) => self.parse_number(*range, *lit, Const::S32),
                    Some((Prim::S64Type, [])) => self.parse_number(*range, *lit, Const::S64),
                    Some((Prim::F32Type, [])) => self.parse_number(*range, *lit, Const::F32),
                    Some((Prim::F64Type, [])) => self.parse_number(*range, *lit, Const::F64),
                    Some((Prim::ReportedError, _)) => None,
                    _ => {
                        self.push_message(Message::NumericLiteralNotSupported {
                            range: file_range,
                            expected_type: self.pretty_value(expected_type),
                        });
                        None
                    }
                };

                match constant {
                    Some(constant) => CheckedPattern::ConstLit(file_range, constant),
                    None => CheckedPattern::ReportedError(file_range),
                }
            }
            Pattern::BooleanLiteral(_, boolean) => {
                let constant = match expected_type.match_prim_spine() {
                    Some((Prim::BoolType, [])) => match *boolean {
                        true => Some(Const::Bool(true)),
                        false => Some(Const::Bool(false)),
                    },
                    _ => {
                        self.push_message(Message::BooleanLiteralNotSupported {
                            range: file_range,
                        });
                        None
                    }
                };

                match constant {
                    Some(constant) => CheckedPattern::ConstLit(file_range, constant),
                    None => CheckedPattern::ReportedError(file_range),
                }
            }
        }
    }

    /// Synthesize the type of a pattern.
    fn synth_pattern(
        &mut self,
        pattern: &Pattern<ByteRange>,
    ) -> (CheckedPattern, ArcValue<'arena>) {
        let file_range = self.file_range(pattern.range());
        match pattern {
            Pattern::Name(_, name) => {
                let source = MetaSource::NamedPatternType(file_range, *name);
                let r#type = self.push_unsolved_type(source);
                (CheckedPattern::Binder(file_range, *name), r#type)
            }
            Pattern::Placeholder(_) => {
                let source = MetaSource::PlaceholderPatternType(file_range);
                let r#type = self.push_unsolved_type(source);
                (CheckedPattern::Placeholder(file_range), r#type)
            }
            Pattern::StringLiteral(_, _) => {
                self.push_message(Message::AmbiguousStringLiteral { range: file_range });
                let source = MetaSource::ReportedErrorType(file_range);
                let r#type = self.push_unsolved_type(source);
                (CheckedPattern::ReportedError(file_range), r#type)
            }
            Pattern::NumberLiteral(_, _) => {
                self.push_message(Message::AmbiguousNumericLiteral { range: file_range });
                let source = MetaSource::ReportedErrorType(file_range);
                let r#type = self.push_unsolved_type(source);
                (CheckedPattern::ReportedError(file_range), r#type)
            }
            Pattern::BooleanLiteral(_, val) => {
                let r#const = Const::Bool(*val);
                let r#type = self.bool_type.clone();
                (CheckedPattern::ConstLit(file_range, r#const), r#type)
            }
        }
    }

    /// Check that the type of an annotated pattern matches an expected type.
    fn check_ann_pattern(
        &mut self,
        pattern: &Pattern<ByteRange>,
        r#type: Option<&Term<'_, ByteRange>>,
        expected_type: &ArcValue<'arena>,
    ) -> CheckedPattern {
        match r#type {
            None => self.check_pattern(pattern, expected_type),
            Some(r#type) => {
                let file_range = self.file_range(r#type.range());
                let r#type = self.check(r#type, &self.universe.clone());
                let r#type = self.eval_env().eval(&r#type);

                match self.unification_context().unify(&r#type, expected_type) {
                    Ok(()) => self.check_pattern(pattern, &r#type),
                    Err(error) => {
                        self.push_message(Message::FailedToUnify {
                            range: file_range,
                            found: self.pretty_value(&r#type),
                            expected: self.pretty_value(expected_type),
                            error,
                        });
                        CheckedPattern::ReportedError(file_range)
                    }
                }
            }
        }
    }

    /// Synthesize the type of an annotated pattern.
    fn synth_ann_pattern(
        &mut self,
        pattern: &Pattern<ByteRange>,
        r#type: Option<&Term<'_, ByteRange>>,
    ) -> (CheckedPattern, core::Term<'arena>, ArcValue<'arena>) {
        match r#type {
            None => {
                let (pattern, type_value) = self.synth_pattern(pattern);
                let r#type = self.quote_env().quote(self.scope, &type_value);
                (pattern, r#type, type_value)
            }
            Some(r#type) => {
                let r#type = self.check(r#type, &self.universe.clone());
                let type_value = self.eval_env().eval(&r#type);
                (self.check_pattern(pattern, &type_value), r#type, type_value)
            }
        }
    }

    /// Report an error if `pattern` is refutable
    fn check_pattern_refutability(&mut self, pattern: &CheckedPattern) {
        if let CheckedPattern::ConstLit(range, _) = pattern {
            self.push_message(Message::RefutablePattern {
                pattern_range: *range,
            });
        }
    }

    /// Elaborate a list of parameters, pushing them onto the context.
    fn synth_and_push_params(
        &mut self,
        mut range: FileRange,
        params: &[Param<ByteRange>],
    ) -> Vec<(Span, Plicity, Option<Symbol>, core::Term<'arena>)> {
        self.local_env.reserve(params.len());

        Vec::from_iter(params.iter().map(|param| {
            let old_range = range;
            range = self.file_range(ByteRange::merge(param.pattern.range(), range.byte_range()));

            let (pattern, r#type, type_value) =
                self.synth_ann_pattern(&param.pattern, param.r#type.as_ref());
            self.check_pattern_refutability(&pattern);

            let name = pattern.name();
            self.local_env.push_param(name, type_value);
            (old_range.into(), param.plicity, name, r#type)
        }))
    }

    fn synth_let_def(
        &mut self,
        def: &LetDef<'_, ByteRange>,
    ) -> (core::LetDef<'arena>, ArcValue<'arena>) {
        let (pattern, r#type, type_value) =
            self.synth_ann_pattern(&def.pattern, def.r#type.as_ref());
        let name = pattern.name();
        self.check_pattern_refutability(&pattern);

        let expr = self.check(&def.expr, &type_value);
        (core::LetDef { name, r#type, expr }, type_value)
    }

    /// Check that a surface term conforms to the given type.
    ///
    /// Returns the elaborated term in the core language.
    fn check(
        &mut self,
        surface_term: &Term<'_, ByteRange>,
        expected_type: &ArcValue<'arena>,
    ) -> core::Term<'arena> {
        let file_range = self.file_range(surface_term.range());
        let expected_type = self.elim_env().force(expected_type);

        match (surface_term, expected_type.as_ref()) {
            (Term::Paren(_, term), _) => self.check(term, &expected_type),
            (Term::Let(_, def, body_expr), _) => {
                let (def, type_value) = self.synth_let_def(def);
                let expr_value = self.eval_env().eval(&def.expr);

                let body_expr = self.with_def(def.name, expr_value, type_value, |this| {
                    this.check(body_expr, &expected_type)
                });

                self.builder().r#let(file_range, def, body_expr)
            }
            (Term::If(_, cond_expr, then_expr, else_expr), _) => {
                let cond_expr = self.check(cond_expr, &self.bool_type.clone());
                let then_expr = self.check(then_expr, &expected_type);
                let else_expr = self.check(else_expr, &expected_type);

                self.builder()
                    .if_then_else(file_range, cond_expr, then_expr, else_expr)
            }
            (Term::Match(range, scrutinee_expr, equations), _) => {
                self.check_match(*range, scrutinee_expr, equations, &expected_type)
            }
            (Term::FunLiteral(range, patterns, body_expr), _) => {
                self.check_fun_lit(*range, patterns, body_expr, &expected_type)
            }
            // Attempt to specialize terms with freshly inserted implicit
            // arguments if an explicit function was expected.
            (_, Value::FunType(Plicity::Explicit, ..)) => {
                let surface_range = surface_term.range();
                let (synth_term, synth_type) = self.synth_and_insert_implicit_apps(surface_term);
                self.coerce(surface_range, synth_term, &synth_type, &expected_type)
            }
            (Term::RecordLiteral(range, expr_fields), Value::RecordType(labels, types)) => {
                // TODO: improve handling of duplicate labels
                if self
                    .check_record_fields(*range, expr_fields, |field| field.label, labels)
                    .is_err()
                {
                    return core::Term::error(file_range);
                }

                let mut types = types.clone();
                let mut expr_fields = expr_fields.iter();
                let mut exprs = SliceVec::new(self.scope, types.len());

                while let Some((expr_field, (r#type, next_types))) =
                    Option::zip(expr_fields.next(), self.elim_env().split_telescope(types))
                {
                    let name_expr = Term::Name(expr_field.label.0, expr_field.label.1);
                    let expr = expr_field.expr.as_ref().unwrap_or(&name_expr);
                    let expr = self.check(expr, &r#type);
                    types = next_types(self.eval_env().eval(&expr));
                    exprs.push(expr);
                }

                core::Term::RecordLit(file_range.into(), labels, exprs.into())
            }
            (Term::Tuple(_, elem_exprs), Value::Universe) => {
                self.local_env.reserve(elem_exprs.len());
                let labels = Symbol::get_tuple_labels(0..elem_exprs.len());
                let labels = self.scope.to_scope_from_iter(labels.iter().copied());

                self.with_scope(|this| {
                    let universe = &this.universe.clone();
                    let types =
                        (this.scope).to_scope_from_iter(elem_exprs.iter().map(|elem_expr| {
                            let r#type = this.check(elem_expr, universe);
                            let type_value = this.eval_env().eval(&r#type);
                            this.local_env.push_param(None, type_value);
                            r#type
                        }));
                    core::Term::RecordType(file_range.into(), labels, types)
                })
            }
            (Term::Tuple(_, elem_exprs), Value::Stuck(Head::Prim(Prim::FormatType), args))
                if args.is_empty() =>
            {
                self.local_env.reserve(elem_exprs.len());
                let labels = Symbol::get_tuple_labels(0..elem_exprs.len());
                let labels = self.scope.to_scope_from_iter(labels.iter().copied());

                self.with_scope(|this| {
                    let format_type = this.format_type.clone();
                    let formats =
                        (this.scope).to_scope_from_iter(elem_exprs.iter().map(|elem_expr| {
                            let format = this.check(elem_expr, &format_type);
                            let format_value = this.eval_env().eval(&format);
                            let r#type = this.elim_env().format_repr(&format_value);
                            this.local_env.push_param(None, r#type);
                            format
                        }));
                    core::Term::FormatRecord(file_range.into(), labels, formats)
                })
            }
            (Term::Tuple(range, elem_exprs), Value::RecordType(labels, types)) => {
                if self
                    .check_tuple_fields(*range, elem_exprs, Term::range, labels)
                    .is_err()
                {
                    return core::Term::error(file_range);
                }

                let mut types = types.clone();
                let mut elem_exprs = elem_exprs.iter();
                let mut exprs = SliceVec::new(self.scope, elem_exprs.len());

                while let Some((elem_expr, (r#type, next_types))) =
                    Option::zip(elem_exprs.next(), self.elim_env().split_telescope(types))
                {
                    let expr = self.check(elem_expr, &r#type);
                    types = next_types(self.eval_env().eval(&expr));
                    exprs.push(expr);
                }

                core::Term::RecordLit(file_range.into(), labels, exprs.into())
            }
            (Term::ArrayLiteral(_, elem_exprs), _) => {
                use crate::core::semantics::Elim::FunApp as App;

                let (len_value, elem_type) = match expected_type.match_prim_spine() {
                    Some((Prim::ArrayType, [App(_, elem_type)])) => (None, elem_type),
                    Some((
                        Prim::Array8Type
                        | Prim::Array16Type
                        | Prim::Array32Type
                        | Prim::Array64Type,
                        [App(_, len), App(_, elem_type)],
                    )) => (Some(len), elem_type),
                    Some((Prim::ReportedError, _)) => return core::Term::error(file_range),
                    _ => {
                        self.push_message(Message::ArrayLiteralNotSupported {
                            range: file_range,
                            expected_type: self.pretty_value(&expected_type),
                        });
                        return core::Term::error(file_range);
                    }
                };

                let len = match len_value.map(|val| val.as_ref()) {
                    None => Some(elem_exprs.len() as u64),
                    Some(Value::ConstLit(Const::U8(len, _))) => Some(*len as u64),
                    Some(Value::ConstLit(Const::U16(len, _))) => Some(*len as u64),
                    Some(Value::ConstLit(Const::U32(len, _))) => Some(*len as u64),
                    Some(Value::ConstLit(Const::U64(len, _))) => Some(*len),
                    Some(Value::Stuck(Head::Prim(Prim::ReportedError), _)) => {
                        return core::Term::error(file_range)
                    }
                    _ => None,
                };

                match len {
                    Some(len) if elem_exprs.len() as u64 == len => core::Term::ArrayLit(
                        file_range.into(),
                        self.scope.to_scope_from_iter(
                            (elem_exprs.iter()).map(|elem_expr| self.check(elem_expr, elem_type)),
                        ),
                    ),
                    _ => {
                        // Check the array elements anyway in order to report
                        // any errors inside the literal as well.
                        for elem_expr in *elem_exprs {
                            self.check(elem_expr, elem_type);
                        }

                        self.push_message(Message::MismatchedArrayLength {
                            range: file_range,
                            found_len: elem_exprs.len(),
                            expected_len: self.pretty_value(len_value.unwrap()),
                        });

                        return core::Term::error(file_range);
                    }
                }
            }
            (Term::StringLiteral(range, lit), _) => {
                let constant = match expected_type.match_prim_spine() {
                    Some((Prim::U8Type, [])) => self.parse_ascii(*range, *lit, Const::U8),
                    Some((Prim::U16Type, [])) => self.parse_ascii(*range, *lit, Const::U16),
                    Some((Prim::U32Type, [])) => self.parse_ascii(*range, *lit, Const::U32),
                    Some((Prim::U64Type, [])) => self.parse_ascii(*range, *lit, Const::U64),
                    // Some((Prim::Array8Type, [len, _])) => todo!(),
                    // Some((Prim::Array16Type, [len, _])) => todo!(),
                    // Some((Prim::Array32Type, [len, _])) => todo!(),
                    // Some((Prim::Array64Type, [len, _])) => todo!(),
                    Some((Prim::ReportedError, _)) => None,
                    _ => {
                        self.push_message(Message::StringLiteralNotSupported {
                            range: file_range,
                            expected_type: self.pretty_value(&expected_type),
                        });
                        None
                    }
                };

                match constant {
                    Some(constant) => core::Term::ConstLit(file_range.into(), constant),
                    None => core::Term::error(file_range),
                }
            }
            (Term::NumberLiteral(range, lit), _) => {
                let constant = match expected_type.match_prim_spine() {
                    Some((Prim::U8Type, [])) => self.parse_number_radix(*range, *lit, Const::U8),
                    Some((Prim::U16Type, [])) => self.parse_number_radix(*range, *lit, Const::U16),
                    Some((Prim::U32Type, [])) => self.parse_number_radix(*range, *lit, Const::U32),
                    Some((Prim::U64Type, [])) => self.parse_number_radix(*range, *lit, Const::U64),
                    Some((Prim::S8Type, [])) => self.parse_number(*range, *lit, Const::S8),
                    Some((Prim::S16Type, [])) => self.parse_number(*range, *lit, Const::S16),
                    Some((Prim::S32Type, [])) => self.parse_number(*range, *lit, Const::S32),
                    Some((Prim::S64Type, [])) => self.parse_number(*range, *lit, Const::S64),
                    Some((Prim::F32Type, [])) => self.parse_number(*range, *lit, Const::F32),
                    Some((Prim::F64Type, [])) => self.parse_number(*range, *lit, Const::F64),
                    Some((Prim::ReportedError, _)) => None,
                    _ => {
                        self.push_message(Message::NumericLiteralNotSupported {
                            range: file_range,
                            expected_type: self.pretty_value(&expected_type),
                        });
                        return core::Term::error(file_range);
                    }
                };

                match constant {
                    Some(constant) => core::Term::ConstLit(file_range.into(), constant),
                    None => core::Term::error(file_range),
                }
            }
            (Term::BinOp(range, lhs, op, rhs), _) => {
                self.check_bin_op(*range, lhs, *op, rhs, &expected_type)
            }
            (Term::ReportedError(_), _) => core::Term::error(file_range),
            (_, _) => {
                let surface_range = surface_term.range();
                let (synth_term, synth_type) = self.synth(surface_term);
                self.coerce(surface_range, synth_term, &synth_type, &expected_type)
            }
        }
    }

    /// Wrap a term in fresh implicit applications that correspond to implicit
    /// parameters in the type provided.
    fn insert_implicit_apps(
        &mut self,
        range: ByteRange,
        mut term: core::Term<'arena>,
        mut r#type: ArcValue<'arena>,
    ) -> (core::Term<'arena>, ArcValue<'arena>) {
        let file_range = self.file_range(range);
        while let Value::FunType(Plicity::Implicit, name, param_type, body_type) =
            self.elim_env().force(&r#type).as_ref()
        {
            let source = MetaSource::ImplicitArg(file_range, *name);
            let arg_term = self.push_unsolved_term(source, param_type.clone());
            let arg_value = self.eval_env().eval(&arg_term);

            term = self
                .builder()
                .fun_app(file_range, Plicity::Implicit, term, arg_term);
            r#type = self.elim_env().apply_closure(body_type, arg_value);
        }
        (term, r#type)
    }

    /// Synthesize the type of `surface_term`, wrapping it in fresh implicit
    /// applications if the term was not an implicit function literal.
    fn synth_and_insert_implicit_apps(
        &mut self,
        surface_term: &Term<'_, ByteRange>,
    ) -> (core::Term<'arena>, ArcValue<'arena>) {
        let (term, r#type) = self.synth(surface_term);
        match term {
            core::Term::FunLit(_, Plicity::Implicit, _, _) => (term, r#type),
            term => self.insert_implicit_apps(surface_term.range(), term, r#type),
        }
    }

    /// Synthesize the type of the given surface term.
    ///
    /// Returns the elaborated term in the core language and its type.
    fn synth(
        &mut self,
        surface_term: &Term<'_, ByteRange>,
    ) -> (core::Term<'arena>, ArcValue<'arena>) {
        let file_range = self.file_range(surface_term.range());
        match surface_term {
            Term::Paren(_, term) => self.synth(term),
            Term::Name(range, name) => {
                if let Some((term, r#type)) = self.get_local_name(*name) {
                    return (
                        core::Term::LocalVar(file_range.into(), term),
                        r#type.clone(),
                    );
                }
                if let Some((term, r#type)) = self.get_item_name(*name) {
                    return (core::Term::ItemVar(file_range.into(), term), r#type.clone());
                }
                if let Some((prim, r#type)) = self.prim_env.get_name(*name) {
                    return (core::Term::Prim(file_range.into(), prim), r#type.clone());
                }

                self.push_message(Message::UnboundName {
                    range: file_range,
                    name: *name,
                    suggested_name: {
                        let item_names = self.item_env.names.iter().copied();
                        let local_names = self.local_env.names.iter().flatten().copied();
                        suggest_name(*name, item_names.chain(local_names))
                    },
                });

                self.synth_reported_error(*range)
            }
            Term::Hole(_, name) => {
                let type_source = MetaSource::HoleType(file_range, *name);
                let expr_source = MetaSource::HoleExpr(file_range, *name);

                let r#type = self.push_unsolved_type(type_source);
                let expr = self.push_unsolved_term(expr_source, r#type.clone());

                (expr, r#type)
            }
            Term::Placeholder(_) => {
                let type_source = MetaSource::PlaceholderType(file_range);
                let expr_source = MetaSource::PlaceholderExpr(file_range);

                let r#type = self.push_unsolved_type(type_source);
                let expr = self.push_unsolved_term(expr_source, r#type.clone());

                (expr, r#type)
            }
            Term::Ann(_, expr, r#type) => {
                let r#type = self.check(r#type, &self.universe.clone());
                let type_value = self.eval_env().eval(&r#type);
                let expr = self.check(expr, &type_value);

                let ann_expr = self.builder().ann(file_range, expr, r#type);
                (ann_expr, type_value)
            }
            Term::Let(_, def, body_expr) => {
                let (def, type_value) = self.synth_let_def(def);
                let expr_value = self.eval_env().eval(&def.expr);

                let (body, body_type) = self.with_def(def.name, expr_value, r#type_value, |this| {
                    this.synth(body_expr)
                });

                let let_expr = self.builder().r#let(file_range, def, body);
                (let_expr, body_type)
            }
            Term::If(_, cond_expr, then_expr, else_expr) => {
                let cond_expr = self.check(cond_expr, &self.bool_type.clone());
                let (then_expr, r#type) = self.synth(then_expr);
                let else_expr = self.check(else_expr, &r#type);

                let match_expr = self
                    .builder()
                    .if_then_else(file_range, cond_expr, then_expr, else_expr);

                (match_expr, r#type)
            }
            Term::Match(range, scrutinee_expr, equations) => {
                // Create a single metavariable representing the overall
                // type of the match expression, allowing us to unify this with
                // the types of the match equations together.
                let r#type = self.push_unsolved_type(MetaSource::MatchExprType(file_range));
                let expr = self.check_match(*range, scrutinee_expr, equations, &r#type);
                (expr, r#type)
            }
            Term::Universe(_) => (
                core::Term::Universe(file_range.into()),
                self.universe.clone(),
            ),
            Term::Arrow(_, plicity, param_type, body_type) => {
                let universe = self.universe.clone();
                let param_type = self.check(param_type, &universe);
                let param_type_value = self.eval_env().eval(&param_type);

                let body_type = self.with_param(None, param_type_value, |this| {
                    this.check(body_type, &universe)
                });

                let fun_type = self
                    .builder()
                    .arrow(file_range, *plicity, param_type, body_type);

                (fun_type, universe)
            }
            Term::FunType(_, params, body_type) => {
                let universe = self.universe.clone();

                let (params, fun_type) = self.with_scope(|this| {
                    let params = this.synth_and_push_params(file_range, params);
                    let fun_type = this.check(body_type, &universe);
                    (params, fun_type)
                });

                // Construct the function type from the parameters
                let fun_type = self.builder().fun_types(params, fun_type);

                (fun_type, universe)
            }
            Term::FunLiteral(range, params, body_expr) => {
                let (expr, r#type) = self.synth_fun_lit(*range, params, body_expr, None);
                (expr, self.eval_env().eval(&r#type))
            }
            Term::App(range, head_expr, args) => {
                let mut head_range = head_expr.range();
                let (mut head_expr, mut head_type) = self.synth(head_expr);

                for arg in *args {
                    head_type = self.elim_env().force(&head_type);

                    match arg.plicity {
                        Plicity::Implicit => {}
                        Plicity::Explicit => {
                            (head_expr, head_type) =
                                self.insert_implicit_apps(head_range, head_expr, head_type);
                        }
                    }

                    let (param_type, body_type) = match head_type.as_ref() {
                        Value::FunType(plicity, _, param_type, body_type) => {
                            if arg.plicity == *plicity {
                                (param_type, body_type)
                            } else {
                                self.messages.push(Message::PlicityArgumentMismatch {
                                    head_range: self.file_range(head_range),
                                    head_plicity: Plicity::Explicit,
                                    head_type: self.pretty_value(&head_type),
                                    arg_range: self.file_range(arg.term.range()),
                                    arg_plicity: arg.plicity,
                                });
                                return self.synth_reported_error(*range);
                            }
                        }

                        // There's been an error when elaborating the head of
                        // the application, so avoid trying to elaborate any
                        // further to prevent cascading type errors.
                        _ if head_expr.is_error() || head_type.is_error() => {
                            return self.synth_reported_error(*range);
                        }
                        _ => {
                            // NOTE: We could try to infer that this is a function type,
                            // but this takes more work to prevent cascading type errors
                            self.push_message(Message::UnexpectedArgument {
                                head_range: self.file_range(head_range),
                                head_type: self.pretty_value(&head_type),
                                arg_range: self.file_range(arg.term.range()),
                            });
                            return self.synth_reported_error(*range);
                        }
                    };

                    let arg_range = arg.term.range();
                    head_range = ByteRange::merge(head_range, arg_range);

                    let arg_expr = self.check(&arg.term, param_type);
                    let arg_expr_value = self.eval_env().eval(&arg_expr);

                    head_expr = self.builder().fun_app(
                        self.file_range(head_range),
                        arg.plicity,
                        head_expr,
                        arg_expr,
                    );
                    head_type = self.elim_env().apply_closure(body_type, arg_expr_value);
                }
                (head_expr, head_type)
            }
            Term::RecordType(range, type_fields) => self.with_scope(|this| {
                let (labels, type_fields) =
                    this.report_duplicate_labels(*range, type_fields, |f| f.label);

                let universe = this.universe.clone();
                let mut types = SliceVec::new(this.scope, labels.len());

                for type_field in type_fields {
                    let r#type = this.check(&type_field.r#type, &universe);
                    let type_value = this.eval_env().eval(&r#type);
                    this.local_env
                        .push_param(Some(type_field.label.1), type_value);
                    types.push(r#type);
                }

                let record_type = core::Term::RecordType(file_range.into(), labels, types.into());
                (record_type, universe)
            }),
            Term::RecordLiteral(range, expr_fields) => {
                let (labels, expr_fields) =
                    self.report_duplicate_labels(*range, expr_fields, |f| f.label);
                let mut types = SliceVec::new(self.scope, labels.len());
                let mut exprs = SliceVec::new(self.scope, labels.len());

                for expr_field in expr_fields {
                    let name_expr = Term::Name(expr_field.label.0, expr_field.label.1);
                    let expr = expr_field.expr.as_ref().unwrap_or(&name_expr);
                    let (expr, r#type) = self.synth(expr);
                    types.push(self.quote_env().quote(self.scope, &r#type));
                    exprs.push(expr);
                }

                let types = Telescope::new(self.local_env.exprs.clone(), types.into());

                (
                    core::Term::RecordLit(file_range.into(), labels, exprs.into()),
                    Spanned::empty(Arc::new(Value::RecordType(labels, types))),
                )
            }
            Term::Tuple(_, elem_exprs) => {
                let labels = Symbol::get_tuple_labels(0..elem_exprs.len());
                let labels = self.scope.to_scope_from_iter(labels.iter().copied());

                let mut exprs = SliceVec::new(self.scope, elem_exprs.len());
                let mut types = SliceVec::new(self.scope, elem_exprs.len());

                for elem_exprs in elem_exprs.iter() {
                    let (expr, r#type) = self.synth(elem_exprs);
                    types.push(self.quote_env().quote(self.scope, &r#type));
                    exprs.push(expr);
                }

                let term = core::Term::RecordLit(file_range.into(), labels, exprs.into());
                let r#type = core::Term::RecordType(Span::Empty, labels, types.into());
                let r#type = self.eval_env().eval(&r#type);

                (term, r#type)
            }
            Term::Proj(range, head_expr, labels) => {
                let head_range = head_expr.range();
                let (mut head_expr, mut head_type) = self.synth_and_insert_implicit_apps(head_expr);

                'labels: for (label_range, proj_label) in *labels {
                    head_type = self.elim_env().force(&head_type);
                    match (&head_expr, head_type.as_ref()) {
                        // Ensure that the head of the projection is a record
                        (_, Value::RecordType(labels, types)) => {
                            let mut labels = labels.iter().copied();
                            let mut types = types.clone();

                            let head_expr_value = self.eval_env().eval(&head_expr);

                            // Look for a field matching the label of the current
                            // projection in the record type.
                            while let Some((label, (r#type, next_types))) =
                                Option::zip(labels.next(), self.elim_env().split_telescope(types))
                            {
                                if *proj_label == label {
                                    // The field was found. Update the head expression
                                    // and continue elaborating the next projection.
                                    head_expr = self.builder().record_proj(
                                        self.file_range(ByteRange::merge(head_range, *label_range)),
                                        head_expr,
                                        *proj_label,
                                    );
                                    head_type = r#type;
                                    continue 'labels;
                                } else {
                                    // This is not the field we are looking for. Substitute the
                                    // value of this field in the rest of the types and continue
                                    // looking for the field.
                                    let head_expr = head_expr_value.clone();
                                    let expr = self.elim_env().record_proj(head_expr, label);
                                    types = next_types(expr);
                                }
                            }
                            // Couldn't find the field in the record type.
                            // Fallthrough with an error.
                        }
                        // There's been an error when elaborating the head of
                        // the projection, so avoid trying to elaborate any
                        // further to prevent cascading type errors.
                        (expr, r#type) if expr.is_error() || r#type.is_error() => {
                            return self.synth_reported_error(*range)
                        }
                        // The head expression was not a record type.
                        // Fallthrough with an error.
                        _ => {}
                    }

                    self.push_message(Message::UnknownField {
                        head_range: self.file_range(head_range),
                        head_type: self.pretty_value(&head_type),
                        label_range: self.file_range(*label_range),
                        label: *proj_label,
                        suggested_label: suggest_name(*proj_label, labels.iter().map(|(_, l)| *l)),
                    });
                    return self.synth_reported_error(*range);
                }

                (head_expr, head_type)
            }
            Term::ArrayLiteral(range, _) => {
                self.push_message(Message::AmbiguousArrayLiteral { range: file_range });
                self.synth_reported_error(*range)
            }
            // TODO: Stuck macros + unification like in Klister?
            Term::StringLiteral(range, _) => {
                self.push_message(Message::AmbiguousStringLiteral { range: file_range });
                self.synth_reported_error(*range)
            }
            // TODO: Stuck macros + unification like in Klister?
            Term::NumberLiteral(range, _) => {
                self.push_message(Message::AmbiguousNumericLiteral { range: file_range });
                self.synth_reported_error(*range)
            }
            Term::BooleanLiteral(_, val) => {
                let expr = core::Term::ConstLit(file_range.into(), Const::Bool(*val));
                (expr, self.bool_type.clone())
            }
            Term::FormatRecord(range, format_fields) => {
                let (labels, formats) = self.check_format_fields(*range, format_fields);
                let format_record = core::Term::FormatRecord(file_range.into(), labels, formats);
                (format_record, self.format_type.clone())
            }
            Term::FormatCond(_, (_, name), format, pred) => {
                let format_type = self.format_type.clone();
                let bool_type = self.bool_type.clone();
                let format = self.check(format, &format_type);
                let format_value = self.eval_env().eval(&format);
                let repr_type = self.elim_env().format_repr(&format_value);

                let pred_expr =
                    self.with_param(*name, repr_type, |this| this.check(pred, &bool_type));

                let cond_format = self
                    .builder()
                    .format_cond(file_range, *name, format, pred_expr);

                (cond_format, format_type)
            }
            Term::FormatOverlap(range, format_fields) => {
                let (labels, formats) = self.check_format_fields(*range, format_fields);
                let overlap_format = core::Term::FormatOverlap(file_range.into(), labels, formats);

                (overlap_format, self.format_type.clone())
            }
            Term::BinOp(range, lhs, op, rhs) => self.synth_bin_op(*range, lhs, *op, rhs),
            Term::ReportedError(range) => self.synth_reported_error(*range),
        }
    }

    fn check_fun_lit(
        &mut self,
        range: ByteRange,
        params: &[Param<'_, ByteRange>],
        body_expr: &Term<'_, ByteRange>,
        expected_type: &ArcValue<'arena>,
    ) -> core::Term<'arena> {
        let file_range = self.file_range(range);
        match params.split_first() {
            Some((param, next_params)) => {
                let body_type = self.elim_env().force(expected_type);
                match body_type.as_ref() {
                    Value::FunType(param_plicity, _, param_type, next_body_type)
                        if param.plicity == *param_plicity =>
                    {
                        let range = ByteRange::merge(param.pattern.range(), body_expr.range());
                        let pattern = self.check_ann_pattern(
                            &param.pattern,
                            param.r#type.as_ref(),
                            param_type,
                        );
                        self.check_pattern_refutability(&pattern);
                        let name = pattern.name();
                        let arg_expr = self.local_env.push_param(name, param_type.clone());

                        let body_type = self.elim_env().apply_closure(next_body_type, arg_expr);
                        let body_expr =
                            self.check_fun_lit(range, next_params, body_expr, &body_type);
                        self.local_env.pop();

                        self.builder().fun_lit(
                            self.file_range(range),
                            param.plicity,
                            name,
                            body_expr,
                        )
                    }
                    // If an implicit function is expected, try to generalize the
                    // function literal by wrapping it in an implicit function
                    Value::FunType(Plicity::Implicit, param_name, param_type, next_body_type)
                        if param.plicity == Plicity::Explicit =>
                    {
                        let arg_expr = self.local_env.push_param(*param_name, param_type.clone());
                        let body_type = self.elim_env().apply_closure(next_body_type, arg_expr);
                        let body_expr = self.check_fun_lit(range, params, body_expr, &body_type);
                        self.local_env.pop();
                        self.builder().fun_lit(
                            file_range,
                            Plicity::Implicit,
                            *param_name,
                            body_expr,
                        )
                    }
                    // Attempt to elaborate the the body of the function in synthesis
                    // mode if we are checking against a metavariable.
                    Value::Stuck(Head::MetaVar(_), _) => {
                        let range = ByteRange::merge(param.pattern.range(), body_expr.range());
                        let (expr, r#type) = self.synth_fun_lit(range, params, body_expr, None);
                        let type_value = self.eval_env().eval(&r#type);
                        self.coerce(range, expr, &type_value, expected_type)
                    }
                    Value::Stuck(Head::Prim(Prim::ReportedError), _) => {
                        core::Term::error(file_range)
                    }
                    _ => {
                        self.push_message(Message::UnexpectedParameter {
                            param_range: self.file_range(param.pattern.range()),
                        });
                        // TODO: For improved error recovery, bind the rest of
                        // the parameters, and check the body of the function
                        // literal using the expected body type.
                        core::Term::error(file_range)
                    }
                }
            }
            None => self.check(body_expr, expected_type),
        }
    }

    fn synth_fun_lit(
        &mut self,
        range: ByteRange,
        params: &[Param<'_, ByteRange>],
        body_expr: &Term<'_, ByteRange>,
        body_type: Option<&Term<'_, ByteRange>>,
    ) -> (core::Term<'arena>, core::Term<'arena>) {
        let file_range = self.file_range(range);
        self.local_env.reserve(params.len());

        let (params, mut fun_lit, mut fun_type) = self.with_scope(|this| {
            let params = this.synth_and_push_params(file_range, params);

            let (fun_lit, fun_type) = match body_type {
                Some(body_type) => {
                    let body_type = this.check(body_type, &this.universe.clone());
                    let body_type_value = this.eval_env().eval(&body_type);
                    (this.check(body_expr, &body_type_value), body_type)
                }
                None => {
                    let (body_expr, body_type) = this.synth(body_expr);
                    (body_expr, this.quote_env().quote(this.scope, &body_type))
                }
            };
            (params, fun_lit, fun_type)
        });

        // Construct the function literal and type from the parameters in reverse
        for (param_range, plicity, name, r#type) in params.into_iter().rev() {
            fun_lit = self.builder().fun_lit(param_range, plicity, name, fun_lit);
            fun_type = self
                .builder()
                .fun_type(Span::Empty, plicity, name, r#type, fun_type);
        }

        (fun_lit, fun_type)
    }

    fn synth_bin_op(
        &mut self,
        range: ByteRange,
        lhs: &Term<'_, ByteRange>,
        op: BinOp<ByteRange>,
        rhs: &Term<'_, ByteRange>,
    ) -> (core::Term<'arena>, ArcValue<'arena>) {
        use BinOp::*;
        use Prim::*;

        // de-sugar into function application
        let (lhs_expr, lhs_type) = self.synth_and_insert_implicit_apps(lhs);
        let (rhs_expr, rhs_type) = self.synth_and_insert_implicit_apps(rhs);
        let lhs_type = self.elim_env().force(&lhs_type);
        let rhs_type = self.elim_env().force(&rhs_type);
        let operand_types = Option::zip(lhs_type.match_prim_spine(), rhs_type.match_prim_spine());

        let (fun, body_type) = match (op, operand_types) {
            (Mul(_), Some(((U8Type, []), (U8Type, [])))) => (U8Mul, U8Type),
            (Mul(_), Some(((U16Type, []), (U16Type, [])))) => (U16Mul, U16Type),
            (Mul(_), Some(((U32Type, []), (U32Type, [])))) => (U32Mul, U32Type),
            (Mul(_), Some(((U64Type, []), (U64Type, [])))) => (U64Mul, U64Type),

            (Mul(_), Some(((S8Type, []), (S8Type, [])))) => (S8Mul, S8Type),
            (Mul(_), Some(((S16Type, []), (S16Type, [])))) => (S16Mul, S16Type),
            (Mul(_), Some(((S32Type, []), (S32Type, [])))) => (S32Mul, S32Type),
            (Mul(_), Some(((S64Type, []), (S64Type, [])))) => (S64Mul, S64Type),

            (Div(_), Some(((U8Type, []), (U8Type, [])))) => (U8Div, U8Type),
            (Div(_), Some(((U16Type, []), (U16Type, [])))) => (U16Div, U16Type),
            (Div(_), Some(((U32Type, []), (U32Type, [])))) => (U32Div, U32Type),
            (Div(_), Some(((U64Type, []), (U64Type, [])))) => (U64Div, U64Type),

            (Div(_), Some(((S8Type, []), (S8Type, [])))) => (S8Div, S8Type),
            (Div(_), Some(((S16Type, []), (S16Type, [])))) => (S16Div, S16Type),
            (Div(_), Some(((S32Type, []), (S32Type, [])))) => (S32Div, S32Type),
            (Div(_), Some(((S64Type, []), (S64Type, [])))) => (S64Div, S64Type),

            (Add(_), Some(((U8Type, []), (U8Type, [])))) => (U8Add, U8Type),
            (Add(_), Some(((U16Type, []), (U16Type, [])))) => (U16Add, U16Type),
            (Add(_), Some(((U32Type, []), (U32Type, [])))) => (U32Add, U32Type),
            (Add(_), Some(((U64Type, []), (U64Type, [])))) => (U64Add, U64Type),

            (Add(_), Some(((S8Type, []), (S8Type, [])))) => (S8Add, S8Type),
            (Add(_), Some(((S16Type, []), (S16Type, [])))) => (S16Add, S16Type),
            (Add(_), Some(((S32Type, []), (S32Type, [])))) => (S32Add, S32Type),
            (Add(_), Some(((S64Type, []), (S64Type, [])))) => (S64Add, S64Type),

            (Add(_), Some(((PosType, []), (U8Type, [])))) => (PosAddU8, PosType),
            (Add(_), Some(((PosType, []), (U16Type, [])))) => (PosAddU16, PosType),
            (Add(_), Some(((PosType, []), (U32Type, [])))) => (PosAddU32, PosType),
            (Add(_), Some(((PosType, []), (U64Type, [])))) => (PosAddU64, PosType),

            (Sub(_), Some(((U8Type, []), (U8Type, [])))) => (U8Sub, U8Type),
            (Sub(_), Some(((U16Type, []), (U16Type, [])))) => (U16Sub, U16Type),
            (Sub(_), Some(((U32Type, []), (U32Type, [])))) => (U32Sub, U32Type),
            (Sub(_), Some(((U64Type, []), (U64Type, [])))) => (U64Sub, U64Type),

            (Sub(_), Some(((S8Type, []), (S8Type, [])))) => (S8Sub, S8Type),
            (Sub(_), Some(((S16Type, []), (S16Type, [])))) => (S16Sub, S16Type),
            (Sub(_), Some(((S32Type, []), (S32Type, [])))) => (S32Sub, S32Type),
            (Sub(_), Some(((S64Type, []), (S64Type, [])))) => (S64Sub, S64Type),

            (Eq(_), Some(((BoolType, []), (BoolType, [])))) => (BoolEq, BoolType),
            (Neq(_), Some(((BoolType, []), (BoolType, [])))) => (BoolNeq, BoolType),

            (Eq(_), Some(((U8Type, []), (U8Type, [])))) => (U8Eq, BoolType),
            (Eq(_), Some(((U16Type, []), (U16Type, [])))) => (U16Eq, BoolType),
            (Eq(_), Some(((U32Type, []), (U32Type, [])))) => (U32Eq, BoolType),
            (Eq(_), Some(((U64Type, []), (U64Type, [])))) => (U64Eq, BoolType),

            (Eq(_), Some(((S8Type, []), (S8Type, [])))) => (S8Eq, BoolType),
            (Eq(_), Some(((S16Type, []), (S16Type, [])))) => (S16Eq, BoolType),
            (Eq(_), Some(((S32Type, []), (S32Type, [])))) => (S32Eq, BoolType),
            (Eq(_), Some(((S64Type, []), (S64Type, [])))) => (S64Eq, BoolType),

            (Neq(_), Some(((U8Type, []), (U8Type, [])))) => (U8Neq, BoolType),
            (Neq(_), Some(((U16Type, []), (U16Type, [])))) => (U16Neq, BoolType),
            (Neq(_), Some(((U32Type, []), (U32Type, [])))) => (U32Neq, BoolType),
            (Neq(_), Some(((U64Type, []), (U64Type, [])))) => (U64Neq, BoolType),

            (Neq(_), Some(((S8Type, []), (S8Type, [])))) => (S8Neq, BoolType),
            (Neq(_), Some(((S16Type, []), (S16Type, [])))) => (S16Neq, BoolType),
            (Neq(_), Some(((S32Type, []), (S32Type, [])))) => (S32Neq, BoolType),
            (Neq(_), Some(((S64Type, []), (S64Type, [])))) => (S64Neq, BoolType),

            (Lt(_), Some(((U8Type, []), (U8Type, [])))) => (U8Lt, BoolType),
            (Lt(_), Some(((U16Type, []), (U16Type, [])))) => (U16Lt, BoolType),
            (Lt(_), Some(((U32Type, []), (U32Type, [])))) => (U32Lt, BoolType),
            (Lt(_), Some(((U64Type, []), (U64Type, [])))) => (U64Lt, BoolType),

            (Lt(_), Some(((S8Type, []), (S8Type, [])))) => (S8Lt, BoolType),
            (Lt(_), Some(((S16Type, []), (S16Type, [])))) => (S16Lt, BoolType),
            (Lt(_), Some(((S32Type, []), (S32Type, [])))) => (S32Lt, BoolType),
            (Lt(_), Some(((S64Type, []), (S64Type, [])))) => (S64Lt, BoolType),

            (Lte(_), Some(((U8Type, []), (U8Type, [])))) => (U8Lte, BoolType),
            (Lte(_), Some(((U16Type, []), (U16Type, [])))) => (U16Lte, BoolType),
            (Lte(_), Some(((U32Type, []), (U32Type, [])))) => (U32Lte, BoolType),
            (Lte(_), Some(((U64Type, []), (U64Type, [])))) => (U64Lte, BoolType),

            (Lte(_), Some(((S8Type, []), (S8Type, [])))) => (S8Lte, BoolType),
            (Lte(_), Some(((S16Type, []), (S16Type, [])))) => (S16Lte, BoolType),
            (Lte(_), Some(((S32Type, []), (S32Type, [])))) => (S32Lte, BoolType),
            (Lte(_), Some(((S64Type, []), (S64Type, [])))) => (S64Lte, BoolType),

            (Gt(_), Some(((U8Type, []), (U8Type, [])))) => (U8Gt, BoolType),
            (Gt(_), Some(((U16Type, []), (U16Type, [])))) => (U16Gt, BoolType),
            (Gt(_), Some(((U32Type, []), (U32Type, [])))) => (U32Gt, BoolType),
            (Gt(_), Some(((U64Type, []), (U64Type, [])))) => (U64Gt, BoolType),

            (Gt(_), Some(((S8Type, []), (S8Type, [])))) => (S8Gt, BoolType),
            (Gt(_), Some(((S16Type, []), (S16Type, [])))) => (S16Gt, BoolType),
            (Gt(_), Some(((S32Type, []), (S32Type, [])))) => (S32Gt, BoolType),
            (Gt(_), Some(((S64Type, []), (S64Type, [])))) => (S64Gt, BoolType),

            (Gte(_), Some(((U8Type, []), (U8Type, [])))) => (U8Gte, BoolType),
            (Gte(_), Some(((U16Type, []), (U16Type, [])))) => (U16Gte, BoolType),
            (Gte(_), Some(((U32Type, []), (U32Type, [])))) => (U32Gte, BoolType),
            (Gte(_), Some(((U64Type, []), (U64Type, [])))) => (U64Gte, BoolType),

            (Gte(_), Some(((S8Type, []), (S8Type, [])))) => (S8Gte, BoolType),
            (Gte(_), Some(((S16Type, []), (S16Type, [])))) => (S16Gte, BoolType),
            (Gte(_), Some(((S32Type, []), (S32Type, [])))) => (S32Gte, BoolType),
            (Gte(_), Some(((S64Type, []), (S64Type, [])))) => (S64Gte, BoolType),

            _ => {
                self.push_message(Message::BinOpMismatchedTypes {
                    range: self.file_range(range),
                    lhs_range: self.file_range(lhs.range()),
                    rhs_range: self.file_range(rhs.range()),
                    op: op.map_range(|range| self.file_range(range)),
                    lhs: self.pretty_value(&lhs_type),
                    rhs: self.pretty_value(&rhs_type),
                });
                return self.synth_reported_error(range);
            }
        };

        let term_span = self.file_range(range);
        let op_span = self.file_range(op.range());

        let fun_app = self
            .builder()
            .binop(term_span, op_span, fun, lhs_expr, rhs_expr);

        // TODO: Maybe it would be good to reuse lhs_type here if body_type is the same
        (
            fun_app,
            Spanned::empty(Arc::new(Value::prim(body_type, []))),
        )
    }

    fn check_bin_op(
        &mut self,
        range: ByteRange,
        lhs: &Term<'_, ByteRange>,
        op: BinOp<ByteRange>,
        rhs: &Term<'_, ByteRange>,
        expected_type: &ArcValue<'arena>,
    ) -> core::Term<'arena> {
        use BinOp::*;
        use Prim::*;

        let prim = match expected_type.as_ref() {
            Value::Stuck(Head::Prim(prim), spine) if spine.is_empty() => prim,
            // TODO: handle metavars?
            _ => {
                let (expr, synth_type) = self.synth_bin_op(range, lhs, op, rhs);
                return self.coerce(range, expr, &synth_type, expected_type);
            }
        };

        let (fun, op_type) = match (op, prim) {
            (Add(_), U8Type) => (U8Add, U8Type),
            (Add(_), U16Type) => (U16Add, U16Type),
            (Add(_), U32Type) => (U32Add, U32Type),
            (Add(_), U64Type) => (U64Add, U64Type),

            (Add(_), S8Type) => (S8Add, S8Type),
            (Add(_), S16Type) => (S16Add, S16Type),
            (Add(_), S32Type) => (S32Add, S32Type),
            (Add(_), S64Type) => (S64Add, S64Type),

            (Sub(_), U8Type) => (U8Sub, U8Type),
            (Sub(_), U16Type) => (U16Sub, U16Type),
            (Sub(_), U32Type) => (U32Sub, U32Type),
            (Sub(_), U64Type) => (U64Sub, U64Type),

            (Sub(_), S8Type) => (S8Sub, S8Type),
            (Sub(_), S16Type) => (S16Sub, S16Type),
            (Sub(_), S32Type) => (S32Sub, S32Type),
            (Sub(_), S64Type) => (S64Sub, S64Type),

            (Mul(_), U8Type) => (U8Mul, U8Type),
            (Mul(_), U16Type) => (U16Mul, U16Type),
            (Mul(_), U32Type) => (U32Mul, U32Type),
            (Mul(_), U64Type) => (U64Mul, U64Type),

            (Mul(_), S8Type) => (S8Mul, S8Type),
            (Mul(_), S16Type) => (S16Mul, S16Type),
            (Mul(_), S32Type) => (S32Mul, S32Type),
            (Mul(_), S64Type) => (S64Mul, S64Type),

            (Div(_), U8Type) => (U8Div, U8Type),
            (Div(_), U16Type) => (U16Div, U16Type),
            (Div(_), U32Type) => (U32Div, U32Type),
            (Div(_), U64Type) => (U64Div, U64Type),

            (Div(_), S8Type) => (S8Div, S8Type),
            (Div(_), S16Type) => (S16Div, S16Type),
            (Div(_), S32Type) => (S32Div, S32Type),
            (Div(_), S64Type) => (S64Div, S64Type),

            _ => {
                let (expr, synth_type) = self.synth_bin_op(range, lhs, op, rhs);
                return self.coerce(range, expr, &synth_type, expected_type);
            }
        };

        let expected_type = Spanned::empty(Arc::new(Value::prim(op_type, [])));

        let lhs_expr = self.check(lhs, &expected_type);
        let rhs_expr = self.check(rhs, &expected_type);

        let term_span = self.file_range(range);
        let op_span = self.file_range(op.range());

        self.builder()
            .binop(term_span, op_span, fun, lhs_expr, rhs_expr)
    }

    fn synth_reported_error(&mut self, range: ByteRange) -> (core::Term<'arena>, ArcValue<'arena>) {
        let file_range = self.file_range(range);
        let expr = core::Term::error(file_range);
        let r#type = self.push_unsolved_type(MetaSource::ReportedErrorType(file_range));
        (expr, r#type)
    }

    /// Check a series of format fields
    fn check_format_fields(
        &mut self,
        range: ByteRange,
        format_fields: &[FormatField<'_, ByteRange>],
    ) -> (&'arena [Symbol], &'arena [core::Term<'arena>]) {
        let universe = self.universe.clone();
        let format_type = self.format_type.clone();

        let initial_local_len = self.local_env.len();
        let (labels, format_fields) =
            self.report_duplicate_labels(range, format_fields, |f| match f {
                FormatField::Format { label, .. } | FormatField::Computed { label, .. } => *label,
            });
        let mut formats = SliceVec::new(self.scope, labels.len());

        for format_field in format_fields {
            match format_field {
                FormatField::Format {
                    label: (label_range, label),
                    format,
                    pred,
                } => {
                    let label_range = self.file_range(*label_range);
                    let format = self.check(format, &format_type);
                    let format_value = self.eval_env().eval(&format);
                    let r#type = self.elim_env().format_repr(&format_value);

                    self.local_env.push_param(Some(*label), r#type);

                    match pred {
                        None => formats.push(format),
                        // Elaborate refined fields to conditional formats
                        Some(pred) => {
                            // Note: No need to push a param, as this was done above,
                            // in preparation for checking the the next format field.
                            let cond_expr = self.check(pred, &self.bool_type.clone());

                            let field_span = Span::merge(&label_range.into(), &cond_expr.span());
                            let format = self
                                .builder()
                                .format_cond(field_span, *label, format, cond_expr);
                            formats.push(format);
                        }
                    }
                }
                FormatField::Computed {
                    label: (label_range, label),
                    r#type,
                    expr,
                } => {
                    let label_range = self.file_range(*label_range);
                    let (expr, r#type, type_value) = match r#type {
                        Some(r#type) => {
                            let r#type = self.check(r#type, &universe);
                            let type_value = self.eval_env().eval(&r#type);
                            (self.check(expr, &type_value), r#type, type_value)
                        }
                        None => {
                            let (expr, type_value) = self.synth_and_insert_implicit_apps(expr);
                            let r#type = self.quote_env().quote(self.scope, &type_value);
                            (expr, r#type, type_value)
                        }
                    };

                    let field_span = Span::merge(&label_range.into(), &expr.span());
                    let format = self.builder().fun_apps(
                        core::Term::Prim(field_span, Prim::FormatSucceed),
                        [
                            (field_span, Plicity::Explicit, r#type),
                            (field_span, Plicity::Explicit, expr),
                        ],
                    );
                    // Assume that `Repr ${type_value} ${expr} = ${type_value}`
                    self.local_env.push_param(Some(*label), type_value);
                    formats.push(format);
                }
            }
        }

        self.local_env.truncate(initial_local_len);

        (labels, formats.into())
    }

    fn check_tuple_fields<F>(
        &mut self,
        range: ByteRange,
        fields: &[F],
        get_range: fn(&F) -> ByteRange,
        expected_labels: &[Symbol],
    ) -> Result<(), ()> {
        if fields.len() == expected_labels.len() {
            return Ok(());
        }

        let mut found_labels = Vec::with_capacity(fields.len());
        let mut fields_iter = fields.iter().enumerate().peekable();
        let mut expected_labels_iter = expected_labels.iter();

        // use the label names from the expected labels
        while let Some(((_, field), label)) =
            Option::zip(fields_iter.peek(), expected_labels_iter.next())
        {
            found_labels.push((self.file_range(get_range(field)), *label));
            fields_iter.next();
        }

        // use numeric labels for excess fields
        for (index, field) in fields_iter {
            found_labels.push((
                self.file_range(get_range(field)),
                Symbol::get_tuple_label(index),
            ));
        }

        self.push_message(Message::MismatchedFieldLabels {
            range: self.file_range(range),
            found_labels,
            expected_labels: expected_labels.to_vec(),
        });
        Err(())
    }

    fn check_record_fields<F>(
        &mut self,
        range: ByteRange,
        fields: &[F],
        get_label: impl Fn(&F) -> (ByteRange, Symbol),
        labels: &'arena [Symbol],
    ) -> Result<(), ()> {
        if fields.len() == labels.len()
            && fields
                .iter()
                .zip(labels.iter())
                .all(|(field, type_label)| get_label(field).1 == *type_label)
        {
            return Ok(());
        }

        // TODO: improve handling of duplicate labels
        self.push_message(Message::MismatchedFieldLabels {
            range: self.file_range(range),
            found_labels: fields
                .iter()
                .map(|field| {
                    let (range, label) = get_label(field);
                    (self.file_range(range), label)
                })
                .collect(),
            expected_labels: labels.to_vec(),
        });
        Err(())
    }

    /// Elaborate a match expression in checking mode
    fn check_match(
        &mut self,
        range: ByteRange,
        scrutinee_expr: &Term<'_, ByteRange>,
        equations: &[(Pattern<ByteRange>, Term<'_, ByteRange>)],
        expected_type: &ArcValue<'arena>,
    ) -> core::Term<'arena> {
        let match_info = MatchInfo {
            range,
            scrutinee: self.synth_scrutinee(scrutinee_expr),
            expected_type: self.elim_env().force(expected_type),
        };

        self.elab_match(&match_info, true, equations.iter())
    }

    fn synth_scrutinee(&mut self, scrutinee_expr: &Term<'_, ByteRange>) -> Scrutinee<'arena> {
        let (expr, r#type) = self.synth_and_insert_implicit_apps(scrutinee_expr);

        Scrutinee {
            range: scrutinee_expr.range(),
            expr: self.scope.to_scope(expr),
            r#type,
        }
    }

    /// Elaborate a pattern match into a case tree in the core language.
    ///
    /// The implementation is based on the algorithm described in Section 5 of
    /// [“The Implementation of Functional Programming Languages”][impl-fpl].
    ///
    /// [impl-fpl]: https://www.microsoft.com/en-us/research/publication/the-implementation-of-functional-programming-languages/
    fn elab_match<'a>(
        &mut self,
        match_info: &MatchInfo<'arena>,
        is_reachable: bool,
        mut equations: impl Iterator<Item = &'a (Pattern<ByteRange>, Term<'a, ByteRange>)>,
    ) -> core::Term<'arena> {
        match equations.next() {
            Some((pattern, body_expr)) => {
                match self.check_pattern(pattern, &match_info.scrutinee.r#type) {
                    // Named patterns are elaborated to let bindings, where the
                    // scrutinee is bound as a definition in the body expression.
                    // Subsequent patterns are unreachable.
                    CheckedPattern::Binder(range, name) => {
                        self.check_match_reachable(is_reachable, range);

                        let def_name = Some(name);
                        let def_expr = self.eval_env().eval(match_info.scrutinee.expr);
                        let def_type_value = match_info.scrutinee.r#type.clone();
                        let def_type = self.quote_env().quote(self.scope, &def_type_value);

                        let body_expr = self.with_def(def_name, def_expr, def_type_value, |this| {
                            this.check(body_expr, &match_info.expected_type)
                        });

                        self.elab_match_unreachable(match_info, equations);

                        self.builder().r#let(
                            Span::merge(&range.into(), &body_expr.span()),
                            core::LetDef {
                                name: def_name,
                                r#type: def_type,
                                expr: match_info.scrutinee.expr.clone(),
                            },
                            body_expr,
                        )
                    }
                    // Placeholder patterns just elaborate to the body
                    // expression. Subsequent patterns are unreachable.
                    CheckedPattern::Placeholder(range) => {
                        self.check_match_reachable(is_reachable, range);

                        let body_expr = self.check(body_expr, &match_info.expected_type);
                        self.elab_match_unreachable(match_info, equations);

                        body_expr
                    }
                    // If we see a constant pattern we should expect a run of
                    // constants, elaborating to a constant elimination.
                    CheckedPattern::ConstLit(range, r#const) => {
                        self.check_match_reachable(is_reachable, range);

                        let body_expr = self.check(body_expr, &match_info.expected_type);
                        let const_equation = (range, r#const, body_expr);

                        self.elab_match_const(match_info, is_reachable, const_equation, equations)
                    }
                    // If we hit an error, propagate it, while still checking
                    // the body expression and the subsequent branches.
                    CheckedPattern::ReportedError(range) => {
                        self.check(body_expr, &match_info.expected_type);
                        self.elab_match_unreachable(match_info, equations);
                        core::Term::error(range)
                    }
                }
            }
            None => self.elab_match_absurd(is_reachable, match_info),
        }
    }

    /// Ensure that this part of a match expression is reachable, reporting
    /// a message if it is not.
    fn check_match_reachable(&mut self, is_reachable: bool, range: FileRange) {
        if !is_reachable {
            self.push_message(Message::UnreachablePattern { range });
        }
    }

    /// Elaborate the equations, expecting a series of constant patterns
    fn elab_match_const<'a>(
        &mut self,
        match_info: &MatchInfo<'arena>,
        is_reachable: bool,
        (const_range, r#const, body_expr): (FileRange, Const, core::Term<'arena>),
        mut equations: impl Iterator<Item = &'a (Pattern<ByteRange>, Term<'a, ByteRange>)>,
    ) -> core::Term<'arena> {
        // The full range of this series of patterns
        let mut full_span = Span::merge(&const_range.into(), &body_expr.span());
        // Temporary vector for accumulating branches
        let mut branches = vec![(r#const, body_expr)];

        // Elaborate a run of constant patterns.
        while let Some((pattern, body_expr)) = equations.next() {
            // Update the range up to the end of the next body expression
            full_span = Span::merge(&full_span, &self.file_range(body_expr.range()).into());

            let pattern = self.check_pattern(pattern, &match_info.scrutinee.r#type);
            match pattern {
                CheckedPattern::ConstLit(range, r#const) => {
                    let body_expr = self.check(body_expr, &match_info.expected_type);

                    // Find insertion index of the branch
                    let insertion_index = branches
                        .binary_search_by(|(probe_const, _)| Const::cmp(probe_const, &r#const));

                    match insertion_index {
                        Ok(_) => self.push_message(Message::UnreachablePattern { range }),
                        Err(index) => {
                            // This has not yet been covered, so it should be reachable.
                            self.check_match_reachable(is_reachable, range);
                            branches.insert(index, (r#const, body_expr));
                        }
                    }

                    if let Some(n) = r#const.num_inhabitants() {
                        if branches.len() as u128 >= n {
                            // The match is exhaustive.
                            // No need to elaborate the rest of the patterns
                            self.elab_match_unreachable(match_info, equations);

                            return core::Term::ConstMatch(
                                full_span,
                                match_info.scrutinee.expr,
                                self.scope.to_scope_from_iter(branches.into_iter()),
                                None,
                            );
                        }
                    }
                }
                CheckedPattern::Binder(_, _)
                | CheckedPattern::Placeholder(_)
                | CheckedPattern::ReportedError(_) => {
                    let name = pattern.name();
                    let range = pattern.range();

                    if !pattern.is_err() {
                        self.check_match_reachable(is_reachable, range);
                        self.elab_match_unreachable(match_info, equations);
                    }

                    let default_expr =
                        self.with_param(name, match_info.scrutinee.r#type.clone(), |this| {
                            this.check(body_expr, &match_info.expected_type)
                        });

                    return core::Term::ConstMatch(
                        full_span,
                        match_info.scrutinee.expr,
                        self.scope.to_scope_from_iter(branches.into_iter()),
                        Some((name, self.scope.to_scope(default_expr))),
                    );
                }
            }
        }

        // Finished all the constant patterns without encountering a default
        // case or an exhaustive match
        let default_expr = self.elab_match_absurd(is_reachable, match_info);

        core::Term::ConstMatch(
            full_span,
            match_info.scrutinee.expr,
            self.scope.to_scope_from_iter(branches.into_iter()),
            Some((None, self.scope.to_scope(default_expr))),
        )
    }

    /// Elaborate unreachable match cases. This is useful for that these cases
    /// are correctly typed, even if they are never actually needed.
    fn elab_match_unreachable<'a>(
        &mut self,
        match_info: &MatchInfo<'arena>,
        equations: impl Iterator<Item = &'a (Pattern<ByteRange>, Term<'a, ByteRange>)>,
    ) {
        self.elab_match(match_info, false, equations);
    }

    /// All the equations have been consumed.
    fn elab_match_absurd(
        &mut self,
        is_reachable: bool,
        match_info: &MatchInfo<'arena>,
    ) -> core::Term<'arena> {
        // Report if we can still reach this point
        if is_reachable {
            // TODO: this should be admitted if the scrutinee type is uninhabited
            self.push_message(Message::NonExhaustiveMatchExpr {
                match_expr_range: self.file_range(match_info.range),
                scrutinee_expr_range: self.file_range(match_info.scrutinee.range),
            });
        }
        core::Term::error(self.file_range(match_info.range))
    }
}

trait FromStrRadix: Sized {
    fn from_str_radix(src: &str, radix: u32) -> Result<Self, std::num::ParseIntError>;
}

macro_rules! impl_from_str_radix {
    ($t:ty) => {
        impl FromStrRadix for $t {
            fn from_str_radix(src: &str, radix: u32) -> Result<Self, std::num::ParseIntError> {
                // calls base implementation, not trait function
                Self::from_str_radix(src, radix)
            }
        }
    };
}

impl_from_str_radix!(u8);
impl_from_str_radix!(u16);
impl_from_str_radix!(u32);
impl_from_str_radix!(u64);

/// Simple patterns that have had some initial elaboration performed on them
#[derive(Debug)]
enum CheckedPattern {
    /// Pattern that binds local variable
    Binder(FileRange, Symbol),
    /// Placeholder patterns that match everything
    Placeholder(FileRange),
    /// Constant literals
    ConstLit(FileRange, Const),
    /// Error sentinel
    ReportedError(FileRange),
}
impl CheckedPattern {
    fn name(&self) -> Option<Symbol> {
        match self {
            CheckedPattern::Binder(_, name) => Some(*name),
            _ => None,
        }
    }

    fn range(&self) -> FileRange {
        match self {
            CheckedPattern::Binder(range, ..)
            | CheckedPattern::Placeholder(range, ..)
            | CheckedPattern::ConstLit(range, ..)
            | CheckedPattern::ReportedError(range, ..) => *range,
        }
    }

    fn is_err(&self) -> bool {
        matches!(self, Self::ReportedError(..))
    }
}

/// Scrutinee of a match expression
struct Scrutinee<'arena> {
    range: ByteRange,
    expr: &'arena core::Term<'arena>,
    r#type: ArcValue<'arena>,
}

struct MatchInfo<'arena> {
    /// The full range of the match expression
    range: ByteRange,
    /// The expression being matched on
    scrutinee: Scrutinee<'arena>,
    /// The expected type of the match arms
    expected_type: ArcValue<'arena>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn checked_pattern_size() {
        assert_eq!(std::mem::size_of::<CheckedPattern>(), 32);
    }
}
