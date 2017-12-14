//! Lowering from the surface syntax to the intermediate representation

use std::rc::Rc;

use name::Named;
use syntax;
use syntax::ast::{binary, host, Field};
use ir::ast::{Definition, Expr, Item, ParseExpr, Path, Program, RepeatBound, Type};
use ir::ast::{RcExpr, RcParseExpr, RcType};
use var::{BindingIndex as Bi, BoundVar, ScopeIndex as Si, Var};

impl<'a> From<&'a syntax::ast::Program> for Program {
    fn from(src: &'a syntax::ast::Program) -> Program {
        let mut program = Program::new();

        for definition in &src.definitions {
            // Begin tracking the path of this definition from the root name of the
            // source definition. This will be appended to in order to provide a
            // fully qualified path through the type definitions, eg:
            // `Foo::field::Entry::Variant2::...`
            let path = Path::new(definition.name.clone());

            let definition = match *definition.ty {
                binary::Type::Abs(_, ref params, ref ty) => Definition {
                    doc: Rc::clone(&definition.doc),
                    params: params.iter().map(|p| p.0.clone()).collect(),
                    item: lower_item(&mut program, &path, ty),
                    path,
                },
                _ => Definition {
                    doc: Rc::clone(&definition.doc),
                    params: vec![],
                    item: lower_item(&mut program, &path, &definition.ty),
                    path,
                },
            };

            program.define(definition);
        }

        program
    }
}

fn lower_item(program: &mut Program, path: &Path, ty: &binary::RcType) -> Item {
    match **ty {
        // Structs and unions that are defined at the top level should
        // get the best names, closest to what the author of the data
        // definition intended!
        binary::Type::Struct(_, ref fields) => Item::Struct(
            lower_row(path, fields, |field_path, ty| {
                lower_ty(program, &field_path, ty)
            }),
            Some(struct_parser(path, fields)),
        ),
        binary::Type::Cond(_, ref options) => Item::Union(
            lower_row(path, options, |option_path, &(_, ref ty)| {
                lower_ty(program, &option_path, ty)
            }),
            Some(cond_parser(path, options)),
        ),
        // Everything else should be an alias
        _ => Item::Alias(lower_ty(program, path, ty)),
    }
}

/// Lower a row to the nominal IR, performing a lowering function for each value
///
/// # Arguments
///
/// * `path` - path to the parent struct or union
/// * `row` - the row of entries to be lowered
/// * `lower_value` - a function that will be called for each entry's
///    corresponding value, appending the name of the entry to `path`
fn lower_row<T, U, F>(path: &Path, row: &[Field<T>], mut lower_value: F) -> Vec<Field<U>>
where
    F: FnMut(Path, &T) -> U,
{
    row.iter()
        .map(|item| {
            let item_path = path.append_child(item.name.clone());
            let ty = lower_value(item_path, &item.value);

            Field {
                doc: Rc::clone(&item.doc),
                name: item.name.clone(),
                value: ty,
            }
        })
        .collect()
}

/// Lower a type variable to an IR type
fn lower_ty_var(var: &Var) -> RcType {
    Rc::new(match *var {
        Var::Bound(Named(ref name, _)) => Type::Path(Path::new(name.to_string()), vec![]),
        Var::Free(_) => unimplemented!(),
    })
}

/// Lower binary types to the nominal format
///
/// # Arguments
///
/// * `program` - the current program. Sub-structs and unions will mutate the
///   program, creating corresponding top-level definitions
/// * `path` - path to the parent struct or union
/// * `ty` - the type to be lowered
fn lower_ty(program: &mut Program, path: &Path, ty: &binary::RcType) -> RcType {
    // Mirroring `binary::Type::repr`
    match **ty {
        binary::Type::Var(_, ref var) => lower_ty_var(var),
        binary::Type::Const(ty_const) => Rc::new(Type::Const(ty_const.repr())),
        binary::Type::Array(_, ref elem_ty, _) => {
            let elem_path = path.append_child("Elem");
            let elem_ty = lower_ty(program, &elem_path, elem_ty);

            Rc::new(Type::Array(elem_ty))
        }
        binary::Type::Assert(_, ref ty, _) => lower_ty(program, path, ty),
        binary::Type::Interp(_, _, _, ref repr_ty) => lower_repr_ty(path, repr_ty),
        binary::Type::Cond(_, ref options) => {
            let definition = Definition {
                doc: "".into(),
                path: path.clone(),
                params: vec![],
                item: Item::Union(
                    lower_row(path, options, |option_path, &(_, ref ty)| {
                        lower_ty(program, &option_path, ty)
                    }),
                    None,
                ),
            };

            program.define(definition);
            Rc::new(Type::Path(path.clone(), vec![]))
        }
        binary::Type::Struct(_, ref fields) => {
            let definition = Definition {
                doc: "".into(),
                path: path.clone(),
                params: vec![],
                item: Item::Struct(
                    lower_row(path, fields, |field_path, ty| {
                        lower_ty(program, &field_path, ty)
                    }),
                    None,
                ),
            };

            program.define(definition);
            Rc::new(Type::Path(path.clone(), vec![]))
        }
        binary::Type::Abs(_, _, _) => {
            // Due to the way our surface syntax is defined, the only type
            // abstractions we should encounter are those that are defined on
            // top-level definitions. Thes should have already been handled in
            // the `From<&'a syntax::ast::Program>` impl for `Program`.
            panic!("ICE: encountered unexpected type abstraction: {:?}", ty)
        }
        binary::Type::App(_, ref ty, ref param_tys) => {
            let lowered_ty = lower_ty(program, path, ty);

            // Replace empty parameter lists on paths with the supplied parameters
            // TODO: This feels rather hacky! I'm sure it will break in non-trivial cases.
            // surely there is a better way to handle this?
            if let Type::Path(ref path, ref params) = *lowered_ty {
                assert!(params.is_empty(), "ICE: Params not empty: {:?}", params);

                let lowered_params = param_tys
                    .iter()
                    .enumerate()
                    .map(|(i, ty)| lower_ty(program, &path.append_child(format!("Arg{}", i)), ty))
                    .collect::<Vec<_>>();

                return Rc::new(Type::Path(path.clone(), lowered_params));
            }

            lowered_ty
        }
    }
}

/// Lower host types to the nominal format
///
/// # Arguments
///
/// * `path` - path to the parent struct or union
/// * `ty` - the type to be lowered
fn lower_repr_ty(path: &Path, ty: &host::RcType) -> RcType {
    match **ty {
        host::Type::Var(ref var) => lower_ty_var(var),
        host::Type::Const(ref ty_const) => Rc::new(Type::Const(ty_const.clone())),
        host::Type::Arrow(ref arg_tys, ref ret_ty) => {
            let arg_repr_tys = arg_tys
                .iter()
                .map(|arg_ty| lower_repr_ty(path, arg_ty))
                .collect();
            let ret_repr_ty = lower_repr_ty(path, ret_ty);

            Rc::new(Type::Arrow(arg_repr_tys, ret_repr_ty))
        }
        host::Type::Array(ref elem_ty) => {
            let elem_path = path.append_child("Elem");
            let elem_ty = lower_repr_ty(&elem_path, elem_ty);

            Rc::new(Type::Array(elem_ty))
        }
        host::Type::Union(_) | host::Type::Struct(_) => {
            // We expect that the repr type has already had a corresponding type
            // generated for it, so instead we just return the current path.
            Rc::new(Type::Path(path.clone(), vec![]))
        }
        host::Type::Abs(_, _) => {
            // Due to the way our surface syntax is defined, the only type
            // abstractions we should encounter are those that are defined on
            // top-level definitions. Thes should have already been handled in
            // the `From<&'a syntax::ast::Program>` impl for `Program`.
            panic!("ICE: encountered unexpected type abstraction: {:?}", ty)
        }
        host::Type::App(ref ty, ref param_tys) => {
            let lowered_ty = lower_repr_ty(path, ty);

            // Replace empty parameter lists on paths with the supplied parameters
            // TODO: This feels rather hacky! I'm sure it will break in non-trivial cases.
            // surely there is a better way to handle this?
            if let Type::Path(ref path, ref params) = *lowered_ty {
                assert!(params.is_empty(), "ICE: Params not empty: {:?}", params);

                let lowered_params = param_tys
                    .iter()
                    .enumerate()
                    .map(|(i, ty)| lower_repr_ty(&path.append_child(format!("Arg{}", i)), ty))
                    .collect::<Vec<_>>();

                return Rc::new(Type::Path(path.clone(), lowered_params));
            }

            lowered_ty
        }
    }
}

/// Lower host expressions to the nominal format
///
/// # Arguments
///
/// * `path` - path to the parent struct or union
/// * `expr` - the expression to be lowered
fn lower_expr(path: &Path, expr: &host::RcExpr) -> RcExpr {
    Rc::new(match **expr {
        host::Expr::Const(_, ref c) => Expr::Const(c.clone()),
        host::Expr::Var(_, ref var) => Expr::Var(var.clone()),
        host::Expr::Unop(_, op, ref expr) => Expr::Unop(op, lower_expr(path, expr)),
        host::Expr::Binop(_, op, ref lhs, ref rhs) => {
            Expr::Binop(op, lower_expr(path, lhs), lower_expr(path, rhs))
        }
        host::Expr::Struct(ref fields) => {
            let lowered_fields = lower_row(path, fields, |field_path, expr| {
                lower_expr(&field_path, expr)
            });

            Expr::Struct(path.clone(), lowered_fields)
        }
        host::Expr::Proj(_, ref expr, ref field_name) => {
            Expr::Proj(lower_expr(path, expr), field_name.clone())
        }
        host::Expr::Intro(_, _, _, _) => unimplemented!(),
        host::Expr::Subscript(_, _, _) => unimplemented!(),
        host::Expr::Cast(_, ref src_expr, ref dst_ty) => {
            Expr::Cast(lower_expr(path, src_expr), lower_repr_ty(path, dst_ty))
        }
        host::Expr::Abs(_, ref params, ref body_expr) => {
            let lowered_params = params
                .iter()
                .map(|&Named(ref name, ref ty)| Named(name.clone(), lower_repr_ty(path, ty)))
                .collect();

            Expr::Abs(lowered_params, lower_expr(path, body_expr))
        }
        host::Expr::App(_, ref fn_expr, ref arg_exprs) => {
            let lowered_arg_exprs = arg_exprs
                .iter()
                .map(|expr| lower_expr(path, expr))
                .collect();

            Expr::App(lower_expr(path, fn_expr), lowered_arg_exprs)
        }
    })
}

/// Create a struct parser for the given fields
///
/// # Arguments
///
/// * `path` - path to the parent struct or union
/// * `fields` - the fields to be used in the parser
fn struct_parser(path: &Path, fields: &[Field<binary::RcType>]) -> RcParseExpr {
    use var::ScopeIndex;

    let lower_to_field_parser = |field: &Field<binary::RcType>| {
        (
            field.name.clone(),
            ty_parser(&path.append_child(field.name.clone()), &field.value),
        )
    };
    let lower_to_expr_field = |field: &Field<binary::RcType>| {
        Field {
            doc: Rc::clone(&field.doc),
            name: field.name.clone(),
            value: Rc::new(Expr::Var(Var::free(field.name.clone()))),
        }
    };

    let parse_exprs = fields.iter().map(lower_to_field_parser);
    let expr_fields = fields.iter().map(lower_to_expr_field);

    let mut named_exprs = Vec::with_capacity(fields.len());
    let mut seen_names = Vec::<String>::with_capacity(fields.len());

    for (name, mut parse_expr) in parse_exprs {
        for (scope, name) in seen_names.iter().rev().enumerate() {
            Rc::make_mut(&mut parse_expr).abstract_names_at(&[name], ScopeIndex(scope as u32));
        }

        seen_names.push(name.clone());
        named_exprs.push(Named(name, parse_expr));
    }

    let mut expr = Expr::Struct(path.clone(), expr_fields.collect());
    for (scope, name) in seen_names.iter().rev().enumerate() {
        expr.abstract_names_at(&[name], ScopeIndex(scope as u32));
    }

    Rc::new(ParseExpr::Sequence(named_exprs, Rc::new(expr)))
}

/// Create a union parser for the given fields
///
/// # Arguments
///
/// * `path` - path to the parent struct or union
/// * `fields` - the fields to be used in the parser
fn cond_parser(path: &Path, options: &[Field<(host::RcExpr, binary::RcType)>]) -> RcParseExpr {
    let lower_option = |option: &Field<(host::RcExpr, binary::RcType)>| {
        let pred_expr = lower_expr(path, &option.value.0);
        let variant_parser = Rc::new(ParseExpr::Sequence(
            vec![Named("x".to_owned(), ty_parser(path, &option.value.1))],
            Rc::new(Expr::Intro(
                path.clone(),
                option.name.clone(),
                // FIXME: generate fresh name?
                Rc::new(Expr::Var(Var::bound("x", BoundVar::new(Si(0), Bi(0))))),
            )),
        ));

        (pred_expr, variant_parser)
    };

    Rc::new(ParseExpr::Cond(options.iter().map(lower_option).collect()))
}

/// Create a parser for the given type
///
/// # Arguments
///
/// * `path` - path to the parent struct or union
/// * `ty` - the binary type to use as a basis for the parser
fn ty_parser(path: &Path, ty: &binary::RcType) -> RcParseExpr {
    match **ty {
        binary::Type::Var(_, ref var) => Rc::new(ParseExpr::Var(var.clone())),
        binary::Type::Const(ty_const) => Rc::new(ParseExpr::Const(ty_const)),
        binary::Type::Array(_, ref elem_ty, ref size_expr) => {
            let elem_path = path.append_child("Elem");
            let elem_parser = ty_parser(&elem_path, elem_ty);
            let size_expr = lower_expr(path, size_expr);

            Rc::new(ParseExpr::Repeat(
                elem_parser,
                RepeatBound::Exact(size_expr),
            ))
        }
        binary::Type::Cond(_, ref options) => cond_parser(path, options),
        binary::Type::Struct(_, ref fields) => struct_parser(path, fields),
        binary::Type::Assert(_, ref ty, ref pred_expr) => {
            let ty_parser = ty_parser(path, ty);
            let pred_expr = lower_expr(path, pred_expr);

            Rc::new(ParseExpr::Assert(ty_parser, pred_expr))
        }
        binary::Type::Interp(_, ref ty, ref conv_expr, _) => {
            let fn_expr = lower_expr(path, conv_expr);
            let parser_expr = ty_parser(path, ty);

            Rc::new(ParseExpr::Apply(fn_expr, parser_expr))
        }
        binary::Type::Abs(_, _, _) => unimplemented!("Abs: {:?}", ty),
        binary::Type::App(_, ref ty, _) => ty_parser(path, ty),
    }
}
