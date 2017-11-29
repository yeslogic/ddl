//! The syntax of our data description language

use std::rc::Rc;

use name::{Name, Named};
use source::Span;
use syntax::ast::{self, host, Field, Substitutions};
use var::{ScopeIndex, Var};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Kind of types
    Type,
    /// Kind of type functions
    ///
    /// For now we only allow type arguments of kind `Type`. We represent this
    /// as an arity count
    Arrow { arity: u32 },
}

impl Kind {
    /// Kind of type functions
    pub fn arrow(arity: u32) -> Kind {
        Kind::Arrow { arity }
    }

    pub fn repr(self) -> host::Kind {
        match self {
            Kind::Type => host::Kind::Type,
            Kind::Arrow { arity } => host::Kind::arrow(arity),
        }
    }
}

/// A type constant in the binary language
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TypeConst {
    U8,
    I8,
    U16Le,
    U24Le,
    U32Le,
    U64Le,
    I16Le,
    I24Le,
    I32Le,
    I64Le,
    F32Le,
    F64Le,
    U16Be,
    U24Be,
    U32Be,
    U64Be,
    I16Be,
    I24Be,
    I32Be,
    I64Be,
    F32Be,
    F64Be,
}

impl TypeConst {
    /// Convert a bianary type constant to its corresponding host representation
    pub fn repr(self) -> host::TypeConst {
        use syntax::ast::host::{FloatType, SignedType, UnsignedType};

        match self {
            TypeConst::U8 => host::TypeConst::Unsigned(UnsignedType::U8),
            TypeConst::I8 => host::TypeConst::Signed(SignedType::I8),
            TypeConst::U16Le | TypeConst::U16Be => host::TypeConst::Unsigned(UnsignedType::U16),
            TypeConst::U24Le | TypeConst::U24Be => host::TypeConst::Unsigned(UnsignedType::U24),
            TypeConst::U32Le | TypeConst::U32Be => host::TypeConst::Unsigned(UnsignedType::U32),
            TypeConst::U64Le | TypeConst::U64Be => host::TypeConst::Unsigned(UnsignedType::U64),
            TypeConst::I16Le | TypeConst::I16Be => host::TypeConst::Signed(SignedType::I16),
            TypeConst::I24Le | TypeConst::I24Be => host::TypeConst::Signed(SignedType::I24),
            TypeConst::I32Le | TypeConst::I32Be => host::TypeConst::Signed(SignedType::I32),
            TypeConst::I64Le | TypeConst::I64Be => host::TypeConst::Signed(SignedType::I64),
            TypeConst::F32Le | TypeConst::F32Be => host::TypeConst::Float(FloatType::F32),
            TypeConst::F64Le | TypeConst::F64Be => host::TypeConst::Float(FloatType::F64),
        }
    }
}

/// A binary type
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type<N> {
    /// A type variable: eg. `T`
    Var(Span, Var<N>),
    /// Type constant
    Const(TypeConst),
    /// An array of the specified type, with a size: eg. `[T; n]`
    Array(Span, RcType<N>, host::RcExpr<N>),
    /// A union of types: eg. `union { field : T, ... }`
    Union(Span, Vec<Field<N, RcType<N>>>),
    /// A struct type, with fields: eg. `struct { variant : T, ... }`
    Struct(Span, Vec<Field<N, RcType<N>>>),
    /// A type that is constrained by a predicate: eg. `T where x => x == 3`
    Assert(Span, RcType<N>, host::RcExpr<N>),
    /// An interpreted type
    Interp(Span, RcType<N>, host::RcExpr<N>, host::RcType<N>),
    /// Type abstraction: eg. `\(a, ..) -> T`
    ///
    /// For now we only allow type arguments of kind `Type`
    Abs(Span, Vec<Named<N, ()>>, RcType<N>),
    /// Type application: eg. `T(U, V)`
    App(Span, RcType<N>, Vec<RcType<N>>),
}

pub type RcType<N> = Rc<Type<N>>;

impl<N: Name> Type<N> {
    /// A struct type, with fields: eg. `struct { field : T, ... }`
    pub fn struct_(span: Span, mut fields: Vec<Field<N, RcType<N>>>) -> Type<N> {
        // We maintain a list of the seen field names. This will allow us to
        // recover the index of these variables as we abstract later fields...
        let mut seen_names = Vec::<N>::with_capacity(fields.len());

        for field in &mut fields {
            for (scope, name) in seen_names.iter().rev().enumerate() {
                Rc::make_mut(&mut field.value)
                    .abstract_names_at(&[name.clone()], ScopeIndex(scope as u32));
            }

            // Record that the field has been 'seen'
            seen_names.push(field.name.clone());
        }

        Type::Struct(span, fields)
    }

    /// Type abstraction: eg. `\(a, ..) -> T`
    ///
    /// For now we only allow type arguments of kind `Type`
    pub fn abs<T1>(span: Span, param_names: &[N], body_ty: T1) -> Type<N>
    where
        T1: Into<RcType<N>>,
    {
        let params = param_names
            .iter()
            .map(|name| Named(name.clone(), ()))
            .collect();

        let mut body_ty = body_ty.into();
        Rc::make_mut(&mut body_ty).abstract_names(param_names);

        Type::Abs(span, params, body_ty)
    }

    /// Attempt to lookup the type of a field
    ///
    /// Returns `None` if the type is not a struct or the field is not
    /// present in the struct.
    pub fn lookup_field(&self, name: &N) -> Option<&RcType<N>> {
        match *self {
            Type::Struct(_, ref fields) => ast::lookup_field(fields, name),
            _ => None,
        }
    }

    /// Attempt to lookup the type of a variant
    ///
    /// Returns `None` if the type is not a union or the field is not
    /// present in the union.
    pub fn lookup_variant(&self, name: &N) -> Option<&RcType<N>> {
        match *self {
            Type::Union(_, ref variants) => ast::lookup_field(variants, name),
            _ => None,
        }
    }

    /// Replace occurrences of the free variables that exist as keys on
    /// `substs` with their corresponding types.
    pub fn substitute(&mut self, substs: &Substitutions<N>) {
        let subst_ty = match *self {
            Type::Var(_, Var::Free(ref name)) => match substs.get(name) {
                None => return,
                Some(ty) => ty.clone(),
            },
            Type::Var(_, Var::Bound(_)) | Type::Const(_) => return,
            Type::Array(_, ref mut elem_ty, ref mut _size_expr) => {
                Rc::make_mut(elem_ty).substitute(substs);
                // Rc::make_mut(size_expr).substitute(substs);
                return;
            }
            Type::Union(_, ref mut variants) => {
                for variant in variants {
                    Rc::make_mut(&mut variant.value).substitute(substs);
                }
                return;
            }
            Type::Struct(_, ref mut fields) => {
                for field in fields.iter_mut() {
                    Rc::make_mut(&mut field.value).substitute(substs);
                }
                return;
            }
            Type::Assert(_, ref mut ty, ref mut _pred) => {
                Rc::make_mut(ty).substitute(substs);
                // Rc::make_mut(pred).substitute(substs);
                return;
            }
            Type::Interp(_, ref mut ty, ref mut _conv, ref mut _repr_ty) => {
                Rc::make_mut(ty).substitute(substs);
                // Rc::make_mut(conv).substitute(substs);
                // Rc::make_mut(repr_ty).substitute(substs);
                return;
            }
            Type::Abs(_, _, ref mut body_ty) => {
                Rc::make_mut(body_ty).substitute(substs);
                return;
            }
            Type::App(_, ref mut fn_ty, ref mut arg_tys) => {
                Rc::make_mut(fn_ty).substitute(substs);

                for arg_ty in arg_tys {
                    Rc::make_mut(arg_ty).substitute(substs);
                }

                return;
            }
        };

        *self = subst_ty.clone();
    }

    pub fn abstract_names_at(&mut self, names: &[N], scope: ScopeIndex) {
        match *self {
            Type::Var(_, ref mut var) => var.abstract_names_at(names, scope),
            Type::Const(_) => {}
            Type::Array(_, ref mut elem_ty, ref mut size_expr) => {
                Rc::make_mut(elem_ty).abstract_names_at(names, scope);
                Rc::make_mut(size_expr).abstract_names_at(names, scope);
            }
            Type::Union(_, ref mut variants) => for variant in variants {
                Rc::make_mut(&mut variant.value).abstract_names_at(names, scope);
            },
            Type::Struct(_, ref mut fields) => for (i, field) in fields.iter_mut().enumerate() {
                Rc::make_mut(&mut field.value).abstract_names_at(names, scope.shift(i as u32));
            },
            Type::Assert(_, ref mut ty, ref mut pred) => {
                Rc::make_mut(ty).abstract_names_at(names, scope);
                Rc::make_mut(pred).abstract_names_at(names, scope.succ());
            }
            Type::Interp(_, ref mut ty, ref mut conv, ref mut repr_ty) => {
                Rc::make_mut(ty).abstract_names_at(names, scope);
                Rc::make_mut(conv).abstract_names_at(names, scope.succ());
                Rc::make_mut(repr_ty).abstract_names_at(names, scope);
            }
            Type::Abs(_, _, ref mut body_ty) => {
                Rc::make_mut(body_ty).abstract_names_at(names, scope.succ());
            }
            Type::App(_, ref mut fn_ty, ref mut arg_tys) => {
                Rc::make_mut(fn_ty).abstract_names_at(names, scope);

                for arg_ty in arg_tys {
                    Rc::make_mut(arg_ty).abstract_names_at(names, scope);
                }
            }
        }
    }

    /// Add one layer of abstraction around the type by replacing all the
    /// free variables in `names` with an appropriate De Bruijn index.
    ///
    /// This results in a one 'dangling' index, and so care must be taken
    /// to wrap it in another type that marks the introduction of a new
    /// scope.
    pub fn abstract_names(&mut self, names: &[N]) {
        self.abstract_names_at(names, ScopeIndex(0));
    }

    fn instantiate_at(&mut self, scope: ScopeIndex, tys: &[RcType<N>]) {
        // FIXME: ensure that expressions are not bound at the same scope
        match *self {
            Type::Var(_, Var::Bound(Named(_, var))) => if var.scope == scope {
                *self = (*tys[var.binding.0 as usize]).clone();
            },
            Type::Var(_, Var::Free(_)) | Type::Const(_) => {}
            Type::Array(_, ref mut elem_ty, _) => {
                Rc::make_mut(elem_ty).instantiate_at(scope, tys);
            }
            Type::Assert(_, ref mut ty, _) => {
                Rc::make_mut(ty).instantiate_at(scope.succ(), tys);
            }
            Type::Interp(_, ref mut ty, _, _) => {
                Rc::make_mut(ty).instantiate_at(scope.succ(), tys);
            }
            Type::Union(_, ref mut variants) => for variant in variants {
                Rc::make_mut(&mut variant.value).instantiate_at(scope, tys);
            },
            Type::Struct(_, ref mut fields) => for (i, field) in fields.iter_mut().enumerate() {
                Rc::make_mut(&mut field.value).instantiate_at(scope.shift(i as u32), tys);
            },
            Type::Abs(_, _, ref mut ty) => {
                Rc::make_mut(ty).instantiate_at(scope.succ(), tys);
            }
            Type::App(_, ref mut ty, ref mut arg_tys) => {
                Rc::make_mut(ty).instantiate_at(scope, tys);

                for arg_ty in arg_tys {
                    Rc::make_mut(arg_ty).instantiate_at(scope, tys);
                }
            }
        }
    }

    /// Remove one layer of abstraction in the type by replacing the
    /// appropriate bound variables with copies of `ty`.
    pub fn instantiate(&mut self, tys: &[RcType<N>]) {
        self.instantiate_at(ScopeIndex(0), tys);
    }

    /// Returns the host representation of the binary type
    pub fn repr(&self) -> host::RcType<N> {
        match *self {
            Type::Var(_, ref v) => Rc::new(host::Type::Var(v.clone())),
            Type::Const(ty_const) => Rc::new(host::Type::Const(ty_const.repr())),
            Type::Array(_, ref elem_ty, _) => Rc::new(host::Type::Array(elem_ty.repr())),
            Type::Assert(_, ref ty, _) => ty.repr(),
            Type::Interp(_, _, _, ref repr_ty) => Rc::clone(repr_ty),
            Type::Union(_, ref variants) => {
                let repr_variants = variants
                    .iter()
                    .map(|variant| {
                        Field {
                            doc: Rc::clone(&variant.doc),
                            name: variant.name.clone(),
                            value: variant.value.repr(),
                        }
                    })
                    .collect();

                Rc::new(host::Type::Union(repr_variants))
            }
            Type::Struct(_, ref fields) => {
                let repr_fields = fields
                    .iter()
                    .map(|field| {
                        Field {
                            doc: Rc::clone(&field.doc),
                            name: field.name.clone(),
                            value: field.value.repr(),
                        }
                    })
                    .collect();

                Rc::new(host::Type::Struct(repr_fields))
            }
            Type::Abs(_, ref params, ref body_ty) => {
                Rc::new(host::Type::Abs(params.clone(), body_ty.repr()))
            }
            Type::App(_, ref fn_ty, ref arg_tys) => {
                let arg_tys = arg_tys.iter().map(|arg| arg.repr()).collect();

                Rc::new(host::Type::App(fn_ty.repr(), arg_tys))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod ty {
        use super::*;

        mod abs {
            use super::*;
            use super::Type as T;

            #[test]
            fn id() {
                // λx. x
                // λ   0
                let ty: Type<&'static str> =
                    T::abs(Span::start(), &["x"], T::Var(Span::start(), Var::free("x")));

                assert_debug_snapshot!(ty_abs_id, ty);
            }

            // Examples from https://en.wikipedia.org/wiki/De_Bruijn_index

            #[test]
            fn k_combinator() {
                // λx.λy. x
                // λ  λ   1
                let ty: Type<&'static str> = T::abs(
                    Span::start(),
                    &["x"],
                    T::abs(Span::start(), &["y"], T::Var(Span::start(), Var::free("x"))),
                );

                assert_debug_snapshot!(ty_abs_k_combinator, ty);
            }

            #[test]
            fn s_combinator() {
                // λx.λy.λz. x z (y z)
                // λ  λ  λ   2 0 (1 0)
                let ty: Type<&'static str> = T::abs(
                    Span::start(),
                    &["x"],
                    T::abs(
                        Span::start(),
                        &["y"],
                        T::abs(
                            Span::start(),
                            &["z"],
                            T::App(
                                Span::start(),
                                Rc::new(T::App(
                                    Span::start(),
                                    Rc::new(T::Var(Span::start(), Var::free("x"))),
                                    vec![Rc::new(T::Var(Span::start(), Var::free("z")))],
                                )),
                                vec![
                                    Rc::new(T::App(
                                        Span::start(),
                                        Rc::new(T::Var(Span::start(), Var::free("y"))),
                                        vec![Rc::new(T::Var(Span::start(), Var::free("z")))],
                                    )),
                                ],
                            ),
                        ),
                    ),
                );

                assert_debug_snapshot!(ty_abs_s_combinator, ty);
            }

            #[test]
            fn complex() {
                // λz.(λy. y (λx. x)) (λx. z x)
                // λ  (λ   0 (λ   0)) (λ   1 0)
                let ty: Type<&'static str> = T::abs(
                    Span::start(),
                    &["z"],
                    T::App(
                        Span::start(),
                        Rc::new(T::abs(
                            Span::start(),
                            &["y"],
                            T::App(
                                Span::start(),
                                Rc::new(T::Var(Span::start(), Var::free("y"))),
                                vec![
                                    Rc::new(T::abs(
                                        Span::start(),
                                        &["x"],
                                        T::Var(Span::start(), Var::free("x")),
                                    )),
                                ],
                            ),
                        )),
                        vec![
                            Rc::new(T::abs(
                                Span::start(),
                                &["x"],
                                T::App(
                                    Span::start(),
                                    Rc::new(T::Var(Span::start(), Var::free("z"))),
                                    vec![Rc::new(T::Var(Span::start(), Var::free("x")))],
                                ),
                            )),
                        ],
                    ),
                );

                assert_debug_snapshot!(ty_abs_complex, ty);
            }
        }
    }
}
