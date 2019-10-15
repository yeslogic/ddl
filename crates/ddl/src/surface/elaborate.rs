//! Elaboration from the surface syntax into the core syntax.
//!
//! Performs the following:
//!
//! - name resolution
//! - desugaring
//! - pattern compilation (TODO)
//! - bidirectional type checking (TODO)
//! - unification (TODO)

use codespan::{FileId, Span};
use codespan_reporting::diagnostic::{Diagnostic, Severity};
use std::collections::HashMap;
use std::sync::Arc;

use crate::{core, diagnostics, surface};

/// Elaborate a module in the surface syntax into the core syntax.
pub fn elaborate_module(
    surface_module: &surface::Module,
    report: &mut dyn FnMut(Diagnostic),
) -> core::Module {
    let item_context = ItemContext::new(surface_module.file_id);
    core::Module {
        file_id: surface_module.file_id,
        doc: surface_module.doc.clone(),
        items: elaborate_items(item_context, &surface_module.items, report),
    }
}

/// Contextual information to be used when elaborating items.
pub struct ItemContext {
    /// The file where these items are defined (for error reporting).
    file_id: FileId,
    /// Labels that have previously been used for items, along with the span
    /// where they were introduced (for error reporting).
    items: HashMap<core::Label, (Span, core::Value)>,
}

impl ItemContext {
    /// Create a new item context.
    pub fn new(file_id: FileId) -> ItemContext {
        ItemContext {
            file_id,
            items: HashMap::new(),
        }
    }

    /// Create a field context based on this item context.
    pub fn field_context(&self) -> FieldContext<'_> {
        FieldContext::new(self.file_id, &self.items)
    }

    /// Create a term context based on this item context.
    pub fn term_context(&self) -> TermContext<'_> {
        TermContext::new(self.file_id, &self.items)
    }
}

/// Elaborate items in the surface syntax into items in the core syntax.
pub fn elaborate_items(
    mut context: ItemContext,
    surface_items: &[surface::Item],
    report: &mut dyn FnMut(Diagnostic),
) -> Vec<core::Item> {
    let mut core_items = Vec::new();

    for item in surface_items.iter() {
        use std::collections::hash_map::Entry;

        match item {
            surface::Item::Alias(alias) => {
                let label = core::Label(alias.name.1.clone());
                let (core_term, ty) = match &alias.ty {
                    Some(surface_ty) => {
                        let context = context.term_context();
                        let core_ty = elaborate_universe(&context, surface_ty, report);
                        let ty = core::semantics::eval(&core_ty);
                        let core_term = check_term(&context, &alias.term, &ty, report);
                        (core::Term::Ann(Arc::new(core_term), Arc::new(core_ty)), ty)
                    }
                    None => synth_term(&context.term_context(), &alias.term, report),
                };

                match context.items.entry(label) {
                    Entry::Vacant(entry) => {
                        let item = core::Alias {
                            span: alias.span,
                            doc: alias.doc.clone(),
                            name: entry.key().clone(),
                            term: core_term,
                        };

                        core_items.push(core::Item::Alias(item));
                        entry.insert((alias.span, ty));
                    }
                    Entry::Occupied(entry) => report(diagnostics::item_redefinition(
                        Severity::Error,
                        context.file_id,
                        entry.key(),
                        alias.span,
                        entry.get().0,
                    )),
                }
            }
            surface::Item::Struct(struct_ty) => {
                let label = core::Label(struct_ty.name.1.clone());
                let field_context = context.field_context();
                let core_fields =
                    elaborate_struct_ty_fields(field_context, &struct_ty.fields, report);

                match context.items.entry(label) {
                    Entry::Vacant(entry) => {
                        let item = core::StructType {
                            span: struct_ty.span,
                            doc: struct_ty.doc.clone(),
                            name: entry.key().clone(),
                            fields: core_fields,
                        };

                        core_items.push(core::Item::Struct(item));
                        entry.insert((struct_ty.span, core::Value::Type));
                    }
                    Entry::Occupied(entry) => report(diagnostics::item_redefinition(
                        Severity::Error,
                        context.file_id,
                        entry.key(),
                        struct_ty.span,
                        entry.get().0,
                    )),
                }
            }
        }
    }

    core_items
}

/// Contextual information to be used when elaborating structure type fields.
pub struct FieldContext<'items> {
    /// The file where these fields are defined (for error reporting).
    file_id: FileId,
    /// Previously elaborated items.
    items: &'items HashMap<core::Label, (Span, core::Value)>,
    /// Labels that have previously been used for fields, along with the span
    /// where they were introduced (for error reporting).
    fields: HashMap<core::Label, Span>,
}

impl<'items> FieldContext<'items> {
    /// Create a new field context.
    pub fn new(
        file_id: FileId,
        items: &'items HashMap<core::Label, (Span, core::Value)>,
    ) -> FieldContext<'items> {
        FieldContext {
            file_id,
            fields: HashMap::new(),
            items,
        }
    }

    /// Create a term context based on this field context.
    pub fn term_context(&self) -> TermContext<'_> {
        TermContext::new(self.file_id, self.items)
    }
}

/// Elaborate structure type fields in the surface syntax into structure type
/// fields in the core syntax.
pub fn elaborate_struct_ty_fields(
    mut context: FieldContext<'_>,
    surface_fields: &[surface::TypeField],
    report: &mut dyn FnMut(Diagnostic),
) -> Vec<core::TypeField> {
    let mut core_fields = Vec::with_capacity(surface_fields.len());

    for field in surface_fields {
        use std::collections::hash_map::Entry;

        let label = core::Label(field.name.1.clone());
        let field_span = Span::merge(field.name.0, field.term.span());
        let ty = check_term(
            &context.term_context(),
            &field.term,
            &core::Value::Type,
            report,
        );

        match context.fields.entry(label) {
            Entry::Vacant(entry) => {
                core_fields.push(core::TypeField {
                    doc: field.doc.clone(),
                    start: field_span.start(),
                    name: entry.key().clone(),
                    term: ty,
                });

                entry.insert(field_span);
            }
            Entry::Occupied(entry) => report(diagnostics::field_redeclaration(
                Severity::Error,
                context.file_id,
                entry.key(),
                field_span,
                *entry.get(),
            )),
        }
    }

    core_fields
}

/// Contextual information to be used when elaborating terms.
pub struct TermContext<'items> {
    /// The file where this term is located (for error reporting).
    file_id: FileId,
    /// Previously elaborated items.
    items: &'items HashMap<core::Label, (Span, core::Value)>,
}

impl<'items> TermContext<'items> {
    /// Create a new term context.
    pub fn new(
        file_id: FileId,
        items: &'items HashMap<core::Label, (Span, core::Value)>,
    ) -> TermContext<'items> {
        TermContext { file_id, items }
    }
}

/// Check that a surface term is a type or kind, and elaborate it into the core syntax.
pub fn elaborate_universe(
    context: &TermContext<'_>,
    surface_term: &surface::Term,
    report: &mut dyn FnMut(Diagnostic),
) -> core::Term {
    match surface_term {
        surface::Term::Var(span, name)
            if !context.items.contains_key("Type") && name.as_str() == "Type" =>
        {
            core::Term::Type(*span)
        }
        surface_term => check_term(context, surface_term, &core::Value::Type, report),
    }
}

/// Check a surface term against the given type, and elaborate it into the core syntax.
pub fn check_term(
    context: &TermContext<'_>,
    surface_term: &surface::Term,
    expected_ty: &core::Value,
    report: &mut dyn FnMut(Diagnostic),
) -> core::Term {
    match (surface_term, expected_ty) {
        (surface::Term::Error(span), _) => core::Term::Error(*span),
        (surface_term, core::Value::Error) => core::Term::Error(surface_term.span()),
        (surface::Term::Paren(_, surface_term), expected_ty) => {
            check_term(context, surface_term, expected_ty, report)
        }
        (surface::Term::NumberLiteral(span, literal), _) => match expected_ty {
            core::Value::IntType => match literal.parse_big_int(context.file_id, report) {
                Some(value) => core::Term::IntConst(*span, value),
                None => core::Term::Error(*span),
            },
            core::Value::F32Type => match literal.parse_float(context.file_id, report) {
                Some(value) => core::Term::F32Const(*span, value),
                None => core::Term::Error(*span),
            },
            core::Value::F64Type => match literal.parse_float(context.file_id, report) {
                Some(value) => core::Term::F64Const(*span, value),
                None => core::Term::Error(*span),
            },
            _ => {
                report(diagnostics::bug::not_yet_implemented(
                    context.file_id,
                    *span,
                    "numeric literasl not suppprted for type",
                ));
                core::Term::Error(surface_term.span())
            }
        },
        (surface_term, expected_ty) => {
            let (core_term, synth_ty) = synth_term(context, surface_term, report);

            if core::semantics::equal(&synth_ty, expected_ty) {
                core_term
            } else {
                report(diagnostics::type_mismatch(
                    Severity::Error,
                    context.file_id,
                    surface_term.span(),
                    expected_ty,
                    &synth_ty,
                ));
                core::Term::Error(surface_term.span())
            }
        }
    }
}

/// Synthesize the type of a surface term, and elaborate it into the core syntax.
pub fn synth_term(
    context: &TermContext<'_>,
    surface_term: &surface::Term,
    report: &mut dyn FnMut(Diagnostic),
) -> (core::Term, core::Value) {
    match surface_term {
        surface::Term::Paren(_, surface_term) => synth_term(context, surface_term, report),
        surface::Term::Ann(surface_term, surface_ty) => {
            let core_ty = elaborate_universe(context, surface_ty, report);
            let ty = core::semantics::eval(&core_ty);
            let core_term = check_term(context, surface_term, &ty, report);
            (core::Term::Ann(Arc::new(core_term), Arc::new(core_ty)), ty)
        }
        surface::Term::Var(span, name) => match context.items.get(name.as_str()) {
            Some((_, ty)) => (
                core::Term::Item(*span, core::Label(name.to_string())),
                ty.clone(),
            ),
            None => match name.as_str() {
                "Type" => {
                    report(diagnostics::type_has_no_type(
                        Severity::Error,
                        context.file_id,
                        *span,
                    ));
                    (core::Term::Error(*span), core::Value::Error)
                }
                "U8" => (core::Term::U8Type(*span), core::Value::Type),
                "U16Le" => (core::Term::U16LeType(*span), core::Value::Type),
                "U16Be" => (core::Term::U16BeType(*span), core::Value::Type),
                "U32Le" => (core::Term::U32LeType(*span), core::Value::Type),
                "U32Be" => (core::Term::U32BeType(*span), core::Value::Type),
                "U64Le" => (core::Term::U64LeType(*span), core::Value::Type),
                "U64Be" => (core::Term::U64BeType(*span), core::Value::Type),
                "S8" => (core::Term::S8Type(*span), core::Value::Type),
                "S16Le" => (core::Term::S16LeType(*span), core::Value::Type),
                "S16Be" => (core::Term::S16BeType(*span), core::Value::Type),
                "S32Le" => (core::Term::S32LeType(*span), core::Value::Type),
                "S32Be" => (core::Term::S32BeType(*span), core::Value::Type),
                "S64Le" => (core::Term::S64LeType(*span), core::Value::Type),
                "S64Be" => (core::Term::S64BeType(*span), core::Value::Type),
                "F32Le" => (core::Term::F32LeType(*span), core::Value::Type),
                "F32Be" => (core::Term::F32BeType(*span), core::Value::Type),
                "F64Le" => (core::Term::F64LeType(*span), core::Value::Type),
                "F64Be" => (core::Term::F64BeType(*span), core::Value::Type),
                "Bool" => (core::Term::BoolType(*span), core::Value::Type),
                "Int" => (core::Term::IntType(*span), core::Value::Type),
                "F32" => (core::Term::F32Type(*span), core::Value::Type),
                "F64" => (core::Term::F64Type(*span), core::Value::Type),
                "true" => (core::Term::BoolConst(*span, true), core::Value::BoolType),
                "false" => (core::Term::BoolConst(*span, false), core::Value::BoolType),
                _ => {
                    report(diagnostics::error::var_name_not_found(
                        context.file_id,
                        name.as_str(),
                        *span,
                    ));

                    (core::Term::Error(*span), core::Value::Error)
                }
            },
        },
        surface::Term::NumberLiteral(_, _) => unimplemented!(),
        surface::Term::Error(span) => (core::Term::Error(*span), core::Value::Error),
    }
}