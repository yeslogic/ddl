//! The operational semantics of the core language, implemented using
//! [normalisation by evaluation](https://en.wikipedia.org/wiki/Normalisation_by_evaluation).

use scoped_arena::Scope;
use std::panic::panic_any;
use std::sync::Arc;

use crate::alloc::SliceVec;
use crate::core::{ConstLit, EntryInfo, Prim, Term, UIntStyle};
use crate::env::{EnvLen, GlobalVar, SharedEnv, SliceEnv};
use crate::StringId;

/// Atomically reference counted values. We use reference counting to increase
/// the amount of sharing we can achieve during evaluation.
pub type ArcValue<'arena> = Arc<Value<'arena>>;

/// Values in weak-head-normal form, with bindings converted to closures.
#[derive(Debug, Clone)]
pub enum Value<'arena> {
    /// A value whose computation has been blocked as a result of trying to
    /// [evaluate][EvalContext::eval] an open [term][Term], along with a spine
    /// of eliminations. Subsequent eliminations applied to this value are
    /// accumulated in the spine.
    Stuck(Head, Vec<Elim<'arena>>),

    /// Universes.
    Universe,

    /// Dependent function types.
    FunType(Option<StringId>, ArcValue<'arena>, Closure<'arena>),
    /// Function literals.
    FunLit(Option<StringId>, Closure<'arena>),

    /// Record types.
    RecordType(&'arena [StringId], Telescope<'arena>),
    /// Record literals.
    RecordLit(&'arena [StringId], Vec<ArcValue<'arena>>),

    /// Array literals.
    ArrayLit(Vec<ArcValue<'arena>>),

    /// Record formats, consisting of a list of dependent formats.
    FormatRecord(&'arena [StringId], Telescope<'arena>),
    /// Overlap formats, consisting of a list of dependent formats, overlapping
    /// in memory.
    FormatOverlap(&'arena [StringId], Telescope<'arena>),

    /// Constant literals.
    ConstLit(ConstLit),
}

impl<'arena> Value<'arena> {
    pub fn prim(prim: Prim, inputs: impl IntoIterator<Item = ArcValue<'arena>>) -> Value<'arena> {
        let inputs = inputs.into_iter().map(Elim::FunApp).collect();
        Value::Stuck(Head::Prim(prim), inputs)
    }

    pub fn rigid_var(global: GlobalVar) -> Value<'arena> {
        Value::Stuck(Head::RigidVar(global), Vec::new())
    }

    pub fn flexible_var(global: GlobalVar) -> Value<'arena> {
        Value::Stuck(Head::FlexibleVar(global), Vec::new())
    }

    pub fn match_prim_spine(&self) -> Option<(Prim, &[Elim<'arena>])> {
        match self {
            Value::Stuck(Head::Prim(prim), spine) => Some((*prim, &spine)),
            _ => None,
        }
    }
}

/// The head of a [stuck value][Value::Stuck].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// Primitives that have not yet been reduced.
    Prim(Prim),
    /// Variables that refer to rigid binders.
    RigidVar(GlobalVar),
    /// Variables that refer to unsolved flexible problems.
    FlexibleVar(GlobalVar), // TODO: Use a RefCell here?
}

/// A pending elimination to be reduced if the [head][Head] of a [stuck
/// value][Value::Stuck] becomes known.
#[derive(Debug, Clone)]
pub enum Elim<'arena> {
    /// Function applications.
    FunApp(ArcValue<'arena>),
    /// Record projections.
    RecordProj(StringId),
    /// Match on a constant.
    ConstMatch(Branches<'arena, ConstLit>),
}

/// A closure is a term that can later be instantiated with a value.
#[derive(Debug, Clone)]
pub struct Closure<'arena> {
    /// Rigid environment where the closed [term][Self.term] is bound. A new
    /// entry will need to be pushed to this environment before evaluating the
    /// term.
    rigid_exprs: SharedEnv<ArcValue<'arena>>,
    /// The term that is closed over.
    term: &'arena Term<'arena>,
}

impl<'arena> Closure<'arena> {
    /// Construct a closure.
    pub fn new(
        rigid_exprs: SharedEnv<ArcValue<'arena>>,
        term: &'arena Term<'arena>,
    ) -> Closure<'arena> {
        Closure { rigid_exprs, term }
    }
}

/// A series of terms where each term might depend on previous terms.
///
/// The term ‘telescope’ was [coined by de Bruijn] to allude to how each
/// variable scopes over subsequent variables in a nested fashion, like how the
/// segments of an “old-fashioned” expandable telescope slide into each other.
///
/// [coined by de Bruijn]: https://doi.org/10.1016/0890-5401(91)90066-B
#[derive(Debug, Clone)]
pub struct Telescope<'arena> {
    /// Rigid environment where the telescope's [terms][Self.terms] are bound.
    rigid_exprs: SharedEnv<ArcValue<'arena>>,
    /// `Repr` should be applied to each term in the telescope.
    apply_repr: bool,
    /// The terms in the telescope.
    terms: &'arena [Term<'arena>],
}

impl<'arena> Telescope<'arena> {
    /// Construct a telescope.
    pub fn new(
        rigid_exprs: SharedEnv<ArcValue<'arena>>,
        terms: &'arena [Term<'arena>],
    ) -> Telescope<'arena> {
        Telescope {
            rigid_exprs,
            apply_repr: false,
            terms,
        }
    }

    fn apply_repr(self) -> Telescope<'arena> {
        debug_assert_eq!(self.apply_repr, false);
        Telescope {
            apply_repr: true,
            ..self
        }
    }

    /// The number of terms in the telescope.
    pub fn len(&self) -> usize {
        self.terms.len()
    }
}

/// The branches of a single-level pattern match.
#[derive(Debug, Clone)]
pub struct Branches<'arena, P> {
    rigid_exprs: SharedEnv<ArcValue<'arena>>,
    pattern_branches: &'arena [(P, Term<'arena>)],
    default_expr: Option<&'arena Term<'arena>>,
}

impl<'arena, P> Branches<'arena, P> {
    /// Construct a single-level pattern match.
    pub fn new(
        rigid_exprs: SharedEnv<ArcValue<'arena>>,
        pattern_branches: &'arena [(P, Term<'arena>)],
        default_expr: Option<&'arena Term<'arena>>,
    ) -> Branches<'arena, P> {
        Branches {
            rigid_exprs,
            pattern_branches,
            default_expr,
        }
    }

    /// The number of pattern branches.
    pub fn num_patterns(&self) -> usize {
        self.pattern_branches.len()
    }
}

pub type PatternBranch<'arena, P> = (P, ArcValue<'arena>);

#[derive(Clone, Debug)]
pub enum SplitBranches<'arena, P> {
    Branch(PatternBranch<'arena, P>, Branches<'arena, P>),
    Default(Closure<'arena>),
    None,
}

/// Errors encountered while interpreting terms.
// TODO: include stack trace(??)
#[derive(Clone, Debug)]
pub enum Error {
    InvalidRigidVar,
    InvalidFlexibleVar,
    InvalidFunctionApp,
    InvalidRecordProj,
    InvalidConstMatch,
    InvalidFormatRepr,
    MissingConstDefault,
}

impl Error {
    pub fn description(&self) -> &str {
        match &self {
            Error::InvalidRigidVar => "invalid rigid variable",
            Error::InvalidFlexibleVar => "invalid flexible variable",
            Error::InvalidFunctionApp => "invalid function application",
            Error::InvalidRecordProj => "invalid record projection",
            Error::InvalidConstMatch => "invalid constant match",
            Error::InvalidFormatRepr => "invalid format repr",
            Error::MissingConstDefault => "missing default expression",
        }
    }
}

/// Evaluation context.
///
/// Like the [`ElimContext`], this allows for the running of computations, but
/// also maintains a rigid environment, allowing for evaluation.
pub struct EvalContext<'arena, 'env> {
    rigid_exprs: &'env mut SharedEnv<ArcValue<'arena>>,
    flexible_exprs: &'env SliceEnv<Option<ArcValue<'arena>>>,
}

impl<'arena, 'env> EvalContext<'arena, 'env> {
    pub fn new(
        rigid_exprs: &'env mut SharedEnv<ArcValue<'arena>>,
        flexible_exprs: &'env SliceEnv<Option<ArcValue<'arena>>>,
    ) -> EvalContext<'arena, 'env> {
        EvalContext {
            rigid_exprs,
            flexible_exprs,
        }
    }

    fn elim_context(&self) -> ElimContext<'arena, 'env> {
        ElimContext::new(self.flexible_exprs)
    }

    /// Fully normalise a term by first [evaluating][EvalContext::eval] it into
    /// a [value][Value], then [quoting it back][QuoteContext::quote] into a
    /// [term][Term].
    pub fn normalise<'out_arena>(
        &mut self,
        scope: &'out_arena Scope<'out_arena>,
        term: &Term<'arena>,
    ) -> Term<'out_arena> {
        QuoteContext::new(scope, self.rigid_exprs.len(), self.flexible_exprs)
            .quote(&self.eval(term))
    }

    /// Evaluate a [term][Term] into a [value][Value].
    ///
    /// This could be loosely thought of as a just-in-time implementation of
    /// closure conversion + partial evaluation (for more discussion see [this
    /// twitter thread](https://twitter.com/brendanzab/status/1423536653658771457)).
    pub fn eval(&mut self, term: &Term<'arena>) -> ArcValue<'arena> {
        match term {
            Term::RigidVar(var) => match self.rigid_exprs.get_local(*var) {
                Some(value) => value.clone(),
                None => panic_any(Error::InvalidRigidVar),
            },
            Term::FlexibleVar(var) => match self.flexible_exprs.get_global(*var) {
                Some(Some(value)) => value.clone(),
                Some(None) => Arc::new(Value::flexible_var(*var)),
                None => panic_any(Error::InvalidFlexibleVar),
            },
            Term::FlexibleInsertion(var, rigid_infos) => {
                let mut head_expr = self.eval(&Term::FlexibleVar(*var));
                for (info, expr) in Iterator::zip(rigid_infos.iter(), self.rigid_exprs.iter()) {
                    head_expr = match info {
                        EntryInfo::Definition => head_expr,
                        EntryInfo::Parameter => {
                            self.elim_context().fun_app(head_expr, expr.clone())
                        }
                    };
                }
                head_expr
            }
            Term::Ann(expr, _) => self.eval(expr),
            Term::Let(_, _, def_expr, output_expr) => {
                let def_expr = self.eval(def_expr);
                self.rigid_exprs.push(def_expr);
                let output_expr = self.eval(output_expr);
                self.rigid_exprs.pop();
                output_expr
            }

            Term::Universe => Arc::new(Value::Universe),

            Term::FunType(input_name, input_type, output_type) => Arc::new(Value::FunType(
                *input_name,
                self.eval(input_type),
                Closure::new(self.rigid_exprs.clone(), output_type),
            )),
            Term::FunLit(input_name, output_expr) => Arc::new(Value::FunLit(
                *input_name,
                Closure::new(self.rigid_exprs.clone(), output_expr),
            )),
            Term::FunApp(head_expr, input_expr) => {
                let head_expr = self.eval(head_expr);
                let input_expr = self.eval(input_expr);
                self.elim_context().fun_app(head_expr, input_expr)
            }

            Term::RecordType(labels, types) => {
                let types = Telescope::new(self.rigid_exprs.clone(), types);
                Arc::new(Value::RecordType(labels, types))
            }
            Term::RecordLit(labels, exprs) => {
                let exprs = exprs.iter().map(|expr| self.eval(expr)).collect();
                Arc::new(Value::RecordLit(labels, exprs))
            }
            Term::RecordProj(head_expr, label) => {
                let head_expr = self.eval(head_expr);
                self.elim_context().record_proj(head_expr, *label)
            }

            Term::ArrayLit(elem_exprs) => {
                let elem_exprs = (elem_exprs.iter())
                    .map(|elem_expr| self.eval(elem_expr))
                    .collect();
                Arc::new(Value::ArrayLit(elem_exprs))
            }

            Term::FormatRecord(labels, formats) => {
                let formats = Telescope::new(self.rigid_exprs.clone(), formats);
                Arc::new(Value::FormatRecord(labels, formats))
            }
            Term::FormatOverlap(labels, formats) => {
                let formats = Telescope::new(self.rigid_exprs.clone(), formats);
                Arc::new(Value::FormatOverlap(labels, formats))
            }

            Term::Prim(prim) => Arc::new(Value::prim(*prim, [])),

            Term::ConstLit(r#const) => Arc::new(Value::ConstLit(*r#const)),
            Term::ConstMatch(head_expr, branches, default_expr) => {
                let head_expr = self.eval(head_expr);
                let branches = Branches::new(self.rigid_exprs.clone(), branches, *default_expr);
                self.elim_context().const_match(head_expr, branches)
            }
        }
    }
}

/// Primitive evaluation step.
type PrimStep =
    for<'arena> fn(&ElimContext<'arena, '_>, &[Elim<'arena>]) -> Option<ArcValue<'arena>>;

macro_rules! step {
    ($context:pat, [$($input:pat),*] => $output:expr) => {
        Some(|$context, spine| match spine {
            [$(Elim::FunApp($input)),*] => Some($output),
            _ => return None,
        })
    };
}

macro_rules! const_step {
    ([$($input:ident : $Input:ident),*] => $output:expr) => {
        step!(_, [$($input),*] => match ($($input.as_ref(),)*) {
            ($(Value::ConstLit(ConstLit::$Input($input, ..)),)*) => Arc::new(Value::ConstLit($output)),
            _ => return None,
        })
    };
    ([$($input:ident , $style:ident : $Input:ident),*] => $output:expr) => {
        step!(_, [$($input),*] => match ($($input.as_ref(),)*) {
            ($(Value::ConstLit(ConstLit::$Input($input, $style)),)*) => Arc::new(Value::ConstLit($output)),
            _ => return None,
        })
    };
}

/// Returns an evaluation step for a primitive, if there is one defined.
#[rustfmt::skip]
fn prim_step(prim: Prim) -> Option<PrimStep> {
    use std::ops::{BitAnd, BitOr, BitXor, Not};

    match prim {
        Prim::FormatRepr => step!(context, [format] => context.format_repr(format)),

        Prim::BoolEq => const_step!([x: Bool, y: Bool] => ConstLit::Bool(x == y)),
        Prim::BoolNeq => const_step!([x: Bool, y: Bool] => ConstLit::Bool(x != y)),
        Prim::BoolNot => const_step!([x: Bool] => ConstLit::Bool(bool::not(*x))),
        Prim::BoolAnd => const_step!([x: Bool, y: Bool] => ConstLit::Bool(*x && *y)),
        Prim::BoolOr => const_step!([x: Bool, y: Bool] => ConstLit::Bool(*x || *y)),
        Prim::BoolXor => const_step!([x: Bool, y: Bool] => ConstLit::Bool(*x ^ *y)),

        Prim::U8Eq => const_step!([x: U8, y: U8] => ConstLit::Bool(x == y)),
        Prim::U8Neq => const_step!([x: U8, y: U8] => ConstLit::Bool(x != y)),
        Prim::U8Gt => const_step!([x: U8, y: U8] => ConstLit::Bool(x > y)),
        Prim::U8Lt => const_step!([x: U8, y: U8] => ConstLit::Bool(x < y)),
        Prim::U8Gte => const_step!([x: U8, y: U8] => ConstLit::Bool(x >= y)),
        Prim::U8Lte => const_step!([x: U8, y: U8] => ConstLit::Bool(x <= y)),
        Prim::U8Add => const_step!([x, xst: U8, y, yst: U8] => ConstLit::U8(u8::checked_add(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U8Sub => const_step!([x, xst: U8, y, yst: U8] => ConstLit::U8(u8::checked_sub(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U8Mul => const_step!([x, xst: U8, y, yst: U8] => ConstLit::U8(u8::checked_mul(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U8Div => const_step!([x, xst: U8, y, yst: U8] => ConstLit::U8(u8::checked_div(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U8Not => const_step!([x, style: U8] => ConstLit::U8(u8::not(*x), *style)),
        Prim::U8Shl => const_step!([x, xst: U8, y, _yst: U8] => ConstLit::U8(u8::checked_shl(*x, u32::from(*y))?, *xst)),
        Prim::U8Shr => const_step!([x, xst: U8, y, _yst: U8] => ConstLit::U8(u8::checked_shr(*x, u32::from(*y))?, *xst)),
        Prim::U8And => const_step!([x, xst: U8, y, yst: U8] => ConstLit::U8(u8::bitand(*x, *y), UIntStyle::merge(*xst, *yst))),
        Prim::U8Or => const_step!([x, xst: U8, y, yst: U8] => ConstLit::U8(u8::bitor(*x, *y), UIntStyle::merge(*xst, *yst))),
        Prim::U8Xor => const_step!([x, xst: U8, y, yst: U8] => ConstLit::U8(u8::bitxor(*x, *y), UIntStyle::merge(*xst, *yst))),

        Prim::U16Eq => const_step!([x: U16, y: U16] => ConstLit::Bool(x == y)),
        Prim::U16Neq => const_step!([x: U16, y: U16] => ConstLit::Bool(x != y)),
        Prim::U16Gt => const_step!([x: U16, y: U16] => ConstLit::Bool(x > y)),
        Prim::U16Lt => const_step!([x: U16, y: U16] => ConstLit::Bool(x < y)),
        Prim::U16Gte => const_step!([x: U16, y: U16] => ConstLit::Bool(x >= y)),
        Prim::U16Lte => const_step!([x: U16, y: U16] => ConstLit::Bool(x <= y)),
        Prim::U16Add => const_step!([x, xst: U16, y, yst: U16] => ConstLit::U16(u16::checked_add(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U16Sub => const_step!([x, xst: U16, y, yst: U16] => ConstLit::U16(u16::checked_sub(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U16Mul => const_step!([x, xst: U16, y, yst: U16] => ConstLit::U16(u16::checked_mul(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U16Div => const_step!([x, xst: U16, y, yst: U16] => ConstLit::U16(u16::checked_div(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U16Not => const_step!([x: U16] => ConstLit::U16(u16::not(*x), UIntStyle::Decimal)),
        Prim::U16Shl => const_step!([x, xst: U16, y, _yst: U8] => ConstLit::U16(u16::checked_shl(*x, u32::from(*y))?, *xst)),
        Prim::U16Shr => const_step!([x, xst: U16, y, _yst: U8] => ConstLit::U16(u16::checked_shr(*x, u32::from(*y))?, *xst)),
        Prim::U16And => const_step!([x, xst: U16, y, yst: U16] => ConstLit::U16(u16::bitand(*x, *y), UIntStyle::merge(*xst, *yst))),
        Prim::U16Or => const_step!([x, xst: U16, y, yst: U16] => ConstLit::U16(u16::bitor(*x, *y), UIntStyle::merge(*xst, *yst))),
        Prim::U16Xor => const_step!([x, xst: U16, y, yst: U16] => ConstLit::U16(u16::bitxor(*x, *y), UIntStyle::merge(*xst, *yst))),

        Prim::U32Eq => const_step!([x: U32, y: U32] => ConstLit::Bool(x == y)),
        Prim::U32Neq => const_step!([x: U32, y: U32] => ConstLit::Bool(x != y)),
        Prim::U32Gt => const_step!([x: U32, y: U32] => ConstLit::Bool(x > y)),
        Prim::U32Lt => const_step!([x: U32, y: U32] => ConstLit::Bool(x < y)),
        Prim::U32Gte => const_step!([x: U32, y: U32] => ConstLit::Bool(x >= y)),
        Prim::U32Lte => const_step!([x: U32, y: U32] => ConstLit::Bool(x <= y)),
        Prim::U32Add => const_step!([x, xst: U32, y, yst: U32] => ConstLit::U32(u32::checked_add(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U32Sub => const_step!([x, xst: U32, y, yst: U32] => ConstLit::U32(u32::checked_sub(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U32Mul => const_step!([x, xst: U32, y, yst: U32] => ConstLit::U32(u32::checked_mul(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U32Div => const_step!([x, xst: U32, y, yst: U32] => ConstLit::U32(u32::checked_div(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U32Not => const_step!([x: U32] => ConstLit::U32(u32::not(*x), UIntStyle::Decimal)),
        Prim::U32Shl => const_step!([x, xst: U32, y, _yst: U8] => ConstLit::U32(u32::checked_shl(*x, u32::from(*y))?, *xst)),
        Prim::U32Shr => const_step!([x, xst: U32, y, _yst: U8] => ConstLit::U32(u32::checked_shr(*x, u32::from(*y))?, *xst)),
        Prim::U32And => const_step!([x, xst: U32, y, yst: U32] => ConstLit::U32(u32::bitand(*x, *y), UIntStyle::merge(*xst, *yst))),
        Prim::U32Or => const_step!([x, xst: U32, y, yst: U32] => ConstLit::U32(u32::bitor(*x, *y), UIntStyle::merge(*xst, *yst))),
        Prim::U32Xor => const_step!([x, xst: U32, y, yst: U32] => ConstLit::U32(u32::bitxor(*x, *y), UIntStyle::merge(*xst, *yst))),

        Prim::U64Eq => const_step!([x: U64, y: U64] => ConstLit::Bool(x == y)),
        Prim::U64Neq => const_step!([x: U64, y: U64] => ConstLit::Bool(x != y)),
        Prim::U64Gt => const_step!([x: U64, y: U64] => ConstLit::Bool(x > y)),
        Prim::U64Lt => const_step!([x: U64, y: U64] => ConstLit::Bool(x < y)),
        Prim::U64Gte => const_step!([x: U64, y: U64] => ConstLit::Bool(x >= y)),
        Prim::U64Lte => const_step!([x: U64, y: U64] => ConstLit::Bool(x <= y)),
        Prim::U64Add => const_step!([x, xst: U64, y, yst: U64] => ConstLit::U64(u64::checked_add(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U64Sub => const_step!([x, xst: U64, y, yst: U64] => ConstLit::U64(u64::checked_sub(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U64Mul => const_step!([x, xst: U64, y, yst: U64] => ConstLit::U64(u64::checked_mul(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U64Div => const_step!([x, xst: U64, y, yst: U64] => ConstLit::U64(u64::checked_div(*x, *y)?, UIntStyle::merge(*xst, *yst))),
        Prim::U64Not => const_step!([x: U64] => ConstLit::U64(u64::not(*x), UIntStyle::Decimal)),
        Prim::U64Shl => const_step!([x, xst: U64, y, _yst: U8] => ConstLit::U64(u64::checked_shl(*x, u32::from(*y))?, *xst)),
        Prim::U64Shr => const_step!([x, xst: U64, y, _yst: U8] => ConstLit::U64(u64::checked_shr(*x, u32::from(*y))?, *xst)),
        Prim::U64And => const_step!([x, xst: U64, y, yst: U64] => ConstLit::U64(u64::bitand(*x, *y), UIntStyle::merge(*xst, *yst))),
        Prim::U64Or => const_step!([x, xst: U64, y, yst: U64] => ConstLit::U64(u64::bitor(*x, *y), UIntStyle::merge(*xst, *yst))),
        Prim::U64Xor => const_step!([x, xst: U64, y, yst: U64] => ConstLit::U64(u64::bitxor(*x, *y), UIntStyle::merge(*xst, *yst))),

        Prim::S8Eq => const_step!([x: S8, y: S8] => ConstLit::Bool(x == y)),
        Prim::S8Neq => const_step!([x: S8, y: S8] => ConstLit::Bool(x != y)),
        Prim::S8Gt => const_step!([x: S8, y: S8] => ConstLit::Bool(x > y)),
        Prim::S8Lt => const_step!([x: S8, y: S8] => ConstLit::Bool(x < y)),
        Prim::S8Gte => const_step!([x: S8, y: S8] => ConstLit::Bool(x >= y)),
        Prim::S8Lte => const_step!([x: S8, y: S8] => ConstLit::Bool(x <= y)),
        Prim::S8Neg => const_step!([x: S8] => ConstLit::S8(i8::checked_neg(*x)?)),
        Prim::S8Add => const_step!([x: S8, y: S8] => ConstLit::S8(i8::checked_add(*x, *y)?)),
        Prim::S8Sub => const_step!([x: S8, y: S8] => ConstLit::S8(i8::checked_sub(*x, *y)?)),
        Prim::S8Mul => const_step!([x: S8, y: S8] => ConstLit::S8(i8::checked_mul(*x, *y)?)),
        Prim::S8Div => const_step!([x: S8, y: S8] => ConstLit::S8(i8::checked_div(*x, *y)?)),
        Prim::S8Abs => const_step!([x: S8] => ConstLit::S8(i8::abs(*x))),
        Prim::S8UAbs => const_step!([x: S8] => ConstLit::U8(i8::unsigned_abs(*x), UIntStyle::Decimal)),

        Prim::S16Eq => const_step!([x: S16, y: S16] => ConstLit::Bool(x == y)),
        Prim::S16Neq => const_step!([x: S16, y: S16] => ConstLit::Bool(x != y)),
        Prim::S16Gt => const_step!([x: S16, y: S16] => ConstLit::Bool(x > y)),
        Prim::S16Lt => const_step!([x: S16, y: S16] => ConstLit::Bool(x < y)),
        Prim::S16Gte => const_step!([x: S16, y: S16] => ConstLit::Bool(x >= y)),
        Prim::S16Lte => const_step!([x: S16, y: S16] => ConstLit::Bool(x <= y)),
        Prim::S16Neg => const_step!([x: S16] => ConstLit::S16(i16::checked_neg(*x)?)),
        Prim::S16Add => const_step!([x: S16, y: S16] => ConstLit::S16(i16::checked_add(*x, *y)?)),
        Prim::S16Sub => const_step!([x: S16, y: S16] => ConstLit::S16(i16::checked_sub(*x, *y)?)),
        Prim::S16Mul => const_step!([x: S16, y: S16] => ConstLit::S16(i16::checked_mul(*x, *y)?)),
        Prim::S16Div => const_step!([x: S16, y: S16] => ConstLit::S16(i16::checked_div(*x, *y)?)),
        Prim::S16Abs => const_step!([x: S16] => ConstLit::S16(i16::abs(*x))),
        Prim::S16UAbs => const_step!([x: S16] => ConstLit::U16(i16::unsigned_abs(*x), UIntStyle::Decimal)),

        Prim::S32Eq => const_step!([x: S32, y: S32] => ConstLit::Bool(x == y)),
        Prim::S32Neq => const_step!([x: S32, y: S32] => ConstLit::Bool(x != y)),
        Prim::S32Gt => const_step!([x: S32, y: S32] => ConstLit::Bool(x > y)),
        Prim::S32Lt => const_step!([x: S32, y: S32] => ConstLit::Bool(x < y)),
        Prim::S32Gte => const_step!([x: S32, y: S32] => ConstLit::Bool(x >= y)),
        Prim::S32Lte => const_step!([x: S32, y: S32] => ConstLit::Bool(x <= y)),
        Prim::S32Neg => const_step!([x: S32] => ConstLit::S32(i32::checked_neg(*x)?)),
        Prim::S32Add => const_step!([x: S32, y: S32] => ConstLit::S32(i32::checked_add(*x, *y)?)),
        Prim::S32Sub => const_step!([x: S32, y: S32] => ConstLit::S32(i32::checked_sub(*x, *y)?)),
        Prim::S32Mul => const_step!([x: S32, y: S32] => ConstLit::S32(i32::checked_mul(*x, *y)?)),
        Prim::S32Div => const_step!([x: S32, y: S32] => ConstLit::S32(i32::checked_div(*x, *y)?)),
        Prim::S32Abs => const_step!([x: S32] => ConstLit::S32(i32::abs(*x))),
        Prim::S32UAbs => const_step!([x: S32] => ConstLit::U32(i32::unsigned_abs(*x), UIntStyle::Decimal)),

        Prim::S64Eq => const_step!([x: S64, y: S64] => ConstLit::Bool(x == y)),
        Prim::S64Neq => const_step!([x: S64, y: S64] => ConstLit::Bool(x != y)),
        Prim::S64Gt => const_step!([x: S64, y: S64] => ConstLit::Bool(x > y)),
        Prim::S64Lt => const_step!([x: S64, y: S64] => ConstLit::Bool(x < y)),
        Prim::S64Gte => const_step!([x: S64, y: S64] => ConstLit::Bool(x >= y)),
        Prim::S64Lte => const_step!([x: S64, y: S64] => ConstLit::Bool(x <= y)),
        Prim::S64Neg => const_step!([x: S64] => ConstLit::S64(i64::checked_neg(*x)?)),
        Prim::S64Add => const_step!([x: S64, y: S64] => ConstLit::S64(i64::checked_add(*x, *y)?)),
        Prim::S64Sub => const_step!([x: S64, y: S64] => ConstLit::S64(i64::checked_sub(*x, *y)?)),
        Prim::S64Mul => const_step!([x: S64, y: S64] => ConstLit::S64(i64::checked_mul(*x, *y)?)),
        Prim::S64Div => const_step!([x: S64, y: S64] => ConstLit::S64(i64::checked_div(*x, *y)?)),
        Prim::S64Abs => const_step!([x: S64] => ConstLit::S64(i64::abs(*x))),
        Prim::S64UAbs => const_step!([x: S64] => ConstLit::U64(i64::unsigned_abs(*x), UIntStyle::Decimal)),

        Prim::OptionFold => step!(context, [_, _, on_none, on_some, option] => {
            match option.match_prim_spine()? {
                (Prim::OptionSome, [Elim::FunApp(value)]) => {
                    context.fun_app(on_some.clone(), value.clone())
                },
                (Prim::OptionNone, []) => on_none.clone(),
                _ => return None,
            }
        }),

        Prim::Array8Find | Prim::Array16Find | Prim::Array32Find | Prim::Array64Find => {
            step!(context, [_, _, pred, array] => match array.as_ref() {
                Value::ArrayLit(elems) => {
                    for elem in elems {
                        match context.fun_app(pred.clone(), elem.clone()).as_ref() {
                            Value::ConstLit(ConstLit::Bool(true)) => {
                                return Some(Arc::new(Value::prim(Prim::OptionSome, [elem.clone()])))
                            },
                            Value::ConstLit(ConstLit::Bool(false)) => {}
                            _ => return None,
                        }
                    }
                    Arc::new(Value::prim(Prim::OptionNone, []))
                }
                _ => return None,
            })
        }

        Prim::PosAddU8 => const_step!([x: Pos, y: U8] => ConstLit::Pos(u64::checked_add(*x, u64::from(*y))?)),
        Prim::PosAddU16 => const_step!([x: Pos, y: U16] => ConstLit::Pos(u64::checked_add(*x, u64::from(*y))?)),
        Prim::PosAddU32 => const_step!([x: Pos, y: U32] => ConstLit::Pos(u64::checked_add(*x, u64::from(*y))?)),
        Prim::PosAddU64 => const_step!([x: Pos, y: U64] => ConstLit::Pos(u64::checked_add(*x, *y)?)),

        _ => None,
    }
}

/// Elimination context.
///
/// Contains enough state to run computations, but does not contain a rigid
/// environment that would be needed for full evaluation.
pub struct ElimContext<'arena, 'env> {
    flexible_exprs: &'env SliceEnv<Option<ArcValue<'arena>>>,
}

impl<'arena, 'env> ElimContext<'arena, 'env> {
    pub fn new(
        flexible_exprs: &'env SliceEnv<Option<ArcValue<'arena>>>,
    ) -> ElimContext<'arena, 'env> {
        ElimContext { flexible_exprs }
    }

    /// Bring a value up-to-date with any new unification solutions that
    /// might now be present at the head of in the given value.
    pub fn force(&self, value: &ArcValue<'arena>) -> ArcValue<'arena> {
        let mut forced_value = value.clone();
        // Attempt to force flexible values until we don't see any more.
        while let Value::Stuck(Head::FlexibleVar(var), spine) = forced_value.as_ref() {
            match self.flexible_exprs.get_global(*var) {
                // Apply the spine to the solution. This might uncover another
                // flexible value so we'll continue looping.
                Some(Some(expr)) => forced_value = self.apply_spine(expr.clone(), spine),
                // There's no solution for this flexible variable yet, meaning
                // that we've forced the value as much as possible for now
                Some(None) => break,
                None => panic_any(Error::InvalidFlexibleVar),
            }
        }
        forced_value
    }

    /// Apply a closure to a value.
    pub fn apply_closure(
        &self,
        closure: &Closure<'arena>,
        value: ArcValue<'arena>,
    ) -> ArcValue<'arena> {
        let mut rigid_exprs = closure.rigid_exprs.clone();
        rigid_exprs.push(value);
        EvalContext::new(&mut rigid_exprs, self.flexible_exprs).eval(closure.term)
    }

    /// Split a telescope into the first value, and a continuation that returns
    /// a telescope containing the rest of the values.
    pub fn split_telescope(
        &self,
        mut telescope: Telescope<'arena>,
    ) -> Option<(
        ArcValue<'arena>,
        impl FnOnce(ArcValue<'arena>) -> Telescope<'arena>,
    )> {
        let (term, terms) = telescope.terms.split_first()?;
        let mut context = EvalContext::new(&mut telescope.rigid_exprs, self.flexible_exprs);
        let value = match telescope.apply_repr {
            true => context.elim_context().format_repr(&context.eval(term)),
            false => context.eval(term),
        };

        Some((value, move |previous_value| {
            telescope.rigid_exprs.push(previous_value);
            telescope.terms = terms;
            telescope
        }))
    }

    pub fn split_branches<P: Copy>(
        &self,
        mut branches: Branches<'arena, P>,
    ) -> SplitBranches<'arena, P> {
        match branches.pattern_branches.split_first() {
            Some(((r#const, output_expr), pattern_branches)) => {
                branches.pattern_branches = pattern_branches;
                let mut context = EvalContext::new(&mut branches.rigid_exprs, self.flexible_exprs);
                SplitBranches::Branch((*r#const, context.eval(output_expr)), branches)
            }
            None => match branches.default_expr {
                Some(default_expr) => {
                    SplitBranches::Default(Closure::new(branches.rigid_exprs, default_expr))
                }
                None => SplitBranches::None,
            },
        }
    }

    /// Apply a function application to an expression, performing
    /// [beta-reduction] if possible.
    ///
    /// [beta-reduction]: https://ncatlab.org/nlab/show/beta-reduction
    pub fn fun_app(
        &self,
        mut head_expr: ArcValue<'arena>,
        input_expr: ArcValue<'arena>,
    ) -> ArcValue<'arena> {
        match Arc::make_mut(&mut head_expr) {
            // Beta-reduction
            Value::FunLit(_, output_expr) => self.apply_closure(output_expr, input_expr),
            // The computation is stuck, preventing further reduction
            Value::Stuck(head, spine) => {
                spine.push(Elim::FunApp(input_expr));

                match head {
                    Head::Prim(prim) => prim_step(*prim)
                        .and_then(|step| step(self, spine))
                        .unwrap_or(head_expr),
                    _ => head_expr,
                }
            }
            _ => panic_any(Error::InvalidFunctionApp),
        }
    }

    /// Apply a record projection to an expression, performing
    /// [beta-reduction] if possible.
    ///
    /// [beta-reduction]: https://ncatlab.org/nlab/show/beta-reduction
    pub fn record_proj(
        &self,
        mut head_expr: ArcValue<'arena>,
        label: StringId,
    ) -> ArcValue<'arena> {
        match Arc::make_mut(&mut head_expr) {
            // Beta-reduction
            Value::RecordLit(labels, exprs) => (labels.iter())
                .position(|current_label| *current_label == label)
                .and_then(|expr_index| exprs.get(expr_index).cloned())
                .unwrap_or_else(|| panic_any(Error::InvalidRecordProj)),
            // The computation is stuck, preventing further reduction
            Value::Stuck(_, spine) => {
                spine.push(Elim::RecordProj(label));
                head_expr
            }
            _ => panic_any(Error::InvalidRecordProj),
        }
    }

    /// Apply a constant match to an expression, performing [beta-reduction] if
    /// possible.
    ///
    /// [beta-reduction]: https://ncatlab.org/nlab/show/beta-reduction
    fn const_match(
        &self,
        mut head_expr: ArcValue<'arena>,
        mut branches: Branches<'arena, ConstLit>,
    ) -> ArcValue<'arena> {
        match Arc::make_mut(&mut head_expr) {
            Value::ConstLit(r#const) => {
                // Try each branch
                for (branch_const, output_expr) in branches.pattern_branches {
                    if r#const == branch_const {
                        return EvalContext::new(&mut branches.rigid_exprs, self.flexible_exprs)
                            .eval(output_expr);
                    }
                }
                // Otherwise call default with `head_expr`
                let mut rigid_exprs = branches.rigid_exprs.clone();
                rigid_exprs.push(head_expr);
                match branches.default_expr {
                    Some(default_expr) => {
                        EvalContext::new(&mut rigid_exprs, self.flexible_exprs).eval(default_expr)
                    }
                    None => panic_any(Error::MissingConstDefault),
                }
            }
            // The computation is stuck, preventing further reduction
            Value::Stuck(_, spine) => {
                spine.push(Elim::ConstMatch(branches));
                head_expr
            }
            _ => panic_any(Error::InvalidConstMatch),
        }
    }

    /// Apply an expression to an elimination spine.
    fn apply_spine(&self, head_expr: ArcValue<'arena>, spine: &[Elim<'arena>]) -> ArcValue<'arena> {
        spine.iter().fold(head_expr, |head_expr, elim| match elim {
            Elim::FunApp(input_expr) => self.fun_app(head_expr, input_expr.clone()),
            Elim::RecordProj(label) => self.record_proj(head_expr, *label),
            Elim::ConstMatch(split) => self.const_match(head_expr, split.clone()),
        })
    }

    /// Find the representation type of a format description.
    pub fn format_repr(&self, format: &ArcValue<'arena>) -> ArcValue<'arena> {
        match format.as_ref() {
            Value::FormatRecord(labels, formats) | Value::FormatOverlap(labels, formats) => {
                Arc::new(Value::RecordType(labels, formats.clone().apply_repr()))
            }
            Value::Stuck(Head::Prim(prim), spine) => match (prim, &spine[..]) {
                (Prim::FormatU8, []) => Arc::new(Value::prim(Prim::U8Type, [])),
                (Prim::FormatU16Be, []) => Arc::new(Value::prim(Prim::U16Type, [])),
                (Prim::FormatU16Le, []) => Arc::new(Value::prim(Prim::U16Type, [])),
                (Prim::FormatU32Be, []) => Arc::new(Value::prim(Prim::U32Type, [])),
                (Prim::FormatU32Le, []) => Arc::new(Value::prim(Prim::U32Type, [])),
                (Prim::FormatU64Be, []) => Arc::new(Value::prim(Prim::U64Type, [])),
                (Prim::FormatU64Le, []) => Arc::new(Value::prim(Prim::U64Type, [])),
                (Prim::FormatS8, []) => Arc::new(Value::prim(Prim::S8Type, [])),
                (Prim::FormatS16Be, []) => Arc::new(Value::prim(Prim::S16Type, [])),
                (Prim::FormatS16Le, []) => Arc::new(Value::prim(Prim::S16Type, [])),
                (Prim::FormatS32Be, []) => Arc::new(Value::prim(Prim::S32Type, [])),
                (Prim::FormatS32Le, []) => Arc::new(Value::prim(Prim::S32Type, [])),
                (Prim::FormatS64Be, []) => Arc::new(Value::prim(Prim::S64Type, [])),
                (Prim::FormatS64Le, []) => Arc::new(Value::prim(Prim::S64Type, [])),
                (Prim::FormatF32Be, []) => Arc::new(Value::prim(Prim::F32Type, [])),
                (Prim::FormatF32Le, []) => Arc::new(Value::prim(Prim::F32Type, [])),
                (Prim::FormatF64Be, []) => Arc::new(Value::prim(Prim::F64Type, [])),
                (Prim::FormatF64Le, []) => Arc::new(Value::prim(Prim::F64Type, [])),
                (Prim::FormatArray8, [Elim::FunApp(len), Elim::FunApp(elem)]) => Arc::new(
                    Value::prim(Prim::Array8Type, [len.clone(), self.format_repr(elem)]),
                ),
                (Prim::FormatArray16, [Elim::FunApp(len), Elim::FunApp(elem)]) => Arc::new(
                    Value::prim(Prim::Array16Type, [len.clone(), self.format_repr(elem)]),
                ),
                (Prim::FormatArray32, [Elim::FunApp(len), Elim::FunApp(elem)]) => Arc::new(
                    Value::prim(Prim::Array32Type, [len.clone(), self.format_repr(elem)]),
                ),
                (Prim::FormatArray64, [Elim::FunApp(len), Elim::FunApp(elem)]) => Arc::new(
                    Value::prim(Prim::Array64Type, [len.clone(), self.format_repr(elem)]),
                ),
                (Prim::FormatLink, [Elim::FunApp(_), Elim::FunApp(elem)]) => {
                    Arc::new(Value::prim(Prim::RefType, [elem.clone()]))
                }
                (Prim::FormatDeref, [Elim::FunApp(elem), Elim::FunApp(_)]) => {
                    self.format_repr(elem)
                }
                (Prim::FormatStreamPos, []) => Arc::new(Value::prim(Prim::PosType, [])),
                (Prim::FormatSucceed, [Elim::FunApp(elem), _]) => elem.clone(),
                (Prim::FormatFail, []) => Arc::new(Value::prim(Prim::VoidType, [])),
                (Prim::FormatUnwrap, [Elim::FunApp(elem), _]) => elem.clone(),
                (Prim::ReportedError, []) => Arc::new(Value::prim(Prim::ReportedError, [])),
                _ => Arc::new(Value::prim(Prim::FormatRepr, [format.clone()])),
            },
            Value::Stuck(_, _) => Arc::new(Value::prim(Prim::FormatRepr, [format.clone()])),
            _ => panic_any(Error::InvalidFormatRepr),
        }
    }
}

/// Quotation context.
///
/// This context keeps track of the length of the environment, allowing for
/// quotation.
#[derive(Clone)]
pub struct QuoteContext<'in_arena, 'out_arena, 'env> {
    scope: &'out_arena Scope<'out_arena>,
    rigid_exprs: EnvLen,
    flexible_exprs: &'env SliceEnv<Option<ArcValue<'in_arena>>>,
}

impl<'in_arena, 'out_arena, 'env> QuoteContext<'in_arena, 'out_arena, 'env> {
    pub fn new(
        scope: &'out_arena Scope<'out_arena>,
        rigid_exprs: EnvLen,
        flexible_exprs: &'env SliceEnv<Option<ArcValue<'in_arena>>>,
    ) -> QuoteContext<'in_arena, 'out_arena, 'env> {
        QuoteContext {
            scope,
            rigid_exprs,
            flexible_exprs,
        }
    }

    fn elim_context<'this>(&'this self) -> ElimContext<'in_arena, 'env> {
        ElimContext::new(self.flexible_exprs)
    }

    fn push_rigid(&mut self) {
        self.rigid_exprs.push();
    }

    fn pop_rigid(&mut self) {
        self.rigid_exprs.pop();
    }

    /// Quote a [value][Value] back into a [term][Term].
    pub fn quote(&mut self, value: &ArcValue<'in_arena>) -> Term<'out_arena> {
        let value = self.elim_context().force(value);
        match value.as_ref() {
            Value::Stuck(head, spine) => {
                let head_expr = match head {
                    Head::Prim(prim) => Term::Prim(*prim),
                    Head::RigidVar(var) => {
                        // FIXME: Unwrap
                        Term::RigidVar(self.rigid_exprs.global_to_local(*var).unwrap())
                    }
                    Head::FlexibleVar(var) => Term::FlexibleVar(*var),
                };

                spine.iter().fold(head_expr, |head_expr, elim| match elim {
                    Elim::FunApp(input_expr) => Term::FunApp(
                        self.scope.to_scope(head_expr),
                        self.scope.to_scope(self.quote(input_expr)),
                    ),
                    Elim::RecordProj(label) => {
                        Term::RecordProj(self.scope.to_scope(head_expr), *label)
                    }
                    Elim::ConstMatch(branches) => {
                        let mut branches = branches.clone();
                        let mut pattern_branches =
                            SliceVec::new(self.scope, branches.num_patterns());

                        let default_expr = loop {
                            match self.elim_context().split_branches(branches) {
                                SplitBranches::Branch((r#const, output_expr), next_branches) => {
                                    pattern_branches.push((r#const, self.quote(&output_expr)));
                                    branches = next_branches;
                                }
                                SplitBranches::Default(default_expr) => {
                                    break Some(self.quote_closure(&default_expr))
                                }
                                SplitBranches::None => break None,
                            }
                        };

                        Term::ConstMatch(
                            self.scope.to_scope(head_expr),
                            pattern_branches.into(),
                            default_expr.map(|expr| self.scope.to_scope(expr) as &_),
                        )
                    }
                })
            }

            Value::Universe => Term::Universe,

            Value::FunType(input_name, input_type, output_type) => {
                let input_type = self.quote(input_type);
                let output_type = self.quote_closure(output_type);

                Term::FunType(
                    *input_name,
                    self.scope.to_scope(input_type),
                    self.scope.to_scope(output_type),
                )
            }
            Value::FunLit(input_name, output_expr) => {
                let output_expr = self.quote_closure(output_expr);

                Term::FunLit(*input_name, self.scope.to_scope(output_expr))
            }

            Value::RecordType(labels, types) => {
                let labels = self.scope.to_scope_from_iter(labels.iter().copied()); // FIXME: avoid copy if this is the same arena?
                let types = self.quote_telescope(types);

                Term::RecordType(labels, types)
            }
            Value::RecordLit(labels, exprs) => {
                let labels = self.scope.to_scope_from_iter(labels.iter().copied()); // FIXME: avoid copy if this is the same arena?
                let exprs =
                    (self.scope).to_scope_from_iter(exprs.iter().map(|expr| self.quote(expr)));

                Term::RecordLit(labels, exprs)
            }
            Value::ArrayLit(elem_exprs) => {
                let elem_exprs = (self.scope)
                    .to_scope_from_iter(elem_exprs.iter().map(|elem_expr| self.quote(elem_expr)));

                Term::ArrayLit(elem_exprs)
            }

            Value::FormatRecord(labels, formats) => {
                let labels = self.scope.to_scope_from_iter(labels.iter().copied()); // FIXME: avoid copy if this is the same arena?
                let formats = self.quote_telescope(formats);

                Term::FormatRecord(labels, formats)
            }
            Value::FormatOverlap(labels, formats) => {
                let labels = self.scope.to_scope_from_iter(labels.iter().copied()); // FIXME: avoid copy if this is the same arena?
                let formats = self.quote_telescope(formats);

                Term::FormatOverlap(labels, formats)
            }

            Value::ConstLit(r#const) => Term::ConstLit(*r#const),
        }
    }

    /// Quote a [closure][Closure] back into a [term][Term].
    fn quote_closure(&mut self, closure: &Closure<'in_arena>) -> Term<'out_arena> {
        let var = Arc::new(Value::rigid_var(self.rigid_exprs.next_global()));
        let value = self.elim_context().apply_closure(closure, var);

        self.push_rigid();
        let term = self.quote(&value);
        self.pop_rigid();

        term
    }

    /// Quote a [telescope][Telescope] back into a slice of [terms][Term].
    fn quote_telescope(
        &mut self,
        telescope: &Telescope<'in_arena>,
    ) -> &'out_arena [Term<'out_arena>] {
        let initial_rigid_len = self.rigid_exprs;
        let mut telescope = telescope.clone();
        let mut terms = SliceVec::new(self.scope, telescope.len());

        while let Some((value, next_telescope)) = self.elim_context().split_telescope(telescope) {
            let var = Arc::new(Value::rigid_var(self.rigid_exprs.next_global()));
            telescope = next_telescope(var);
            terms.push(self.quote(&value));
            self.rigid_exprs.push();
        }

        self.rigid_exprs.truncate(initial_rigid_len);
        terms.into()
    }
}

/// Conversion context.
///
/// This context keeps track of the length of the environment, for use in
/// conversion checking.
pub struct ConversionContext<'arena, 'env> {
    rigid_exprs: EnvLen,
    flexible_exprs: &'env SliceEnv<Option<ArcValue<'arena>>>,
}

impl<'arena, 'env> ConversionContext<'arena, 'env> {
    pub fn new(
        rigid_exprs: EnvLen,
        flexible_exprs: &'env SliceEnv<Option<ArcValue<'arena>>>,
    ) -> ConversionContext<'arena, 'env> {
        ConversionContext {
            rigid_exprs,
            flexible_exprs,
        }
    }

    fn elim_context(&self) -> ElimContext<'arena, 'env> {
        ElimContext::new(self.flexible_exprs)
    }

    fn push_rigid(&mut self) {
        self.rigid_exprs.push();
    }

    fn pop_rigid(&mut self) {
        self.rigid_exprs.pop();
    }

    /// Check that one value is [computationally equal] to another value.
    ///
    /// This is sometimes referred to as 'conversion checking', or checking
    /// for 'definitional equality'.
    ///
    /// We perform [eta-conversion] here, if possible.
    ///
    /// [computationally equal]: https://ncatlab.org/nlab/show/equality#computational_equality
    /// [eta-conversion]: https://ncatlab.org/nlab/show/eta-conversion
    pub fn is_equal(&mut self, value0: &ArcValue<'_>, value1: &ArcValue<'_>) -> bool {
        let value0 = self.elim_context().force(value0);
        let value1 = self.elim_context().force(value1);

        match (value0.as_ref(), value1.as_ref()) {
            // `ReportedError`s result from errors that have already been
            // reported, so we prevent them from triggering more errors.
            (Value::Stuck(Head::Prim(Prim::ReportedError), _), _)
            | (_, Value::Stuck(Head::Prim(Prim::ReportedError), _)) => true,

            (Value::Stuck(head0, spine0), Value::Stuck(head1, spine1)) => {
                use Elim::*;

                head0 == head1
                    && spine0.len() == spine1.len()
                    && Iterator::zip(spine0.iter(), spine1.iter()).all(|(elim0, elim1)| {
                        match (elim0, elim1) {
                            (FunApp(expr0), FunApp(expr1)) => self.is_equal(expr0, expr1),
                            (RecordProj(label0), RecordProj(label1)) => label0 == label1,
                            (ConstMatch(branches0), ConstMatch(branches1)) => {
                                self.is_equal_branches(branches0, branches1)
                            }
                            (_, _) => false,
                        }
                    })
            }
            (Value::Universe, Value::Universe) => true,

            (
                Value::FunType(_, input_type0, output_type0),
                Value::FunType(_, input_type1, output_type1),
            ) => {
                self.is_equal(input_type0, input_type1)
                    && self.is_equal_closures(output_type0, output_type1)
            }
            (Value::FunLit(_, output_expr0), Value::FunLit(_, output_expr1)) => {
                self.is_equal_closures(output_expr0, output_expr1)
            }
            (Value::FunLit(_, output_expr), _) => self.is_equal_fun_lit(output_expr, &value1),
            (_, Value::FunLit(_, output_expr)) => self.is_equal_fun_lit(output_expr, &value0),

            (Value::RecordType(labels0, types0), Value::RecordType(labels1, types1)) => {
                labels0 == labels1 && self.is_equal_telescopes(types0, types1)
            }
            (Value::RecordLit(labels0, exprs0), Value::RecordLit(labels1, exprs1)) => {
                labels0 == labels1
                    && Iterator::zip(exprs0.iter(), exprs1.iter())
                        .all(|(expr0, expr1)| self.is_equal(&expr0, &expr1))
            }
            (Value::RecordLit(labels, exprs), _) => {
                self.is_equal_record_lit(labels, exprs, &value1)
            }
            (_, Value::RecordLit(labels, exprs)) => {
                self.is_equal_record_lit(labels, exprs, &value0)
            }

            (Value::ArrayLit(elem_exprs0), Value::ArrayLit(elem_exprs1)) => {
                Iterator::zip(elem_exprs0.iter(), elem_exprs1.iter())
                    .all(|(elem_expr0, elem_expr1)| self.is_equal(&elem_expr0, &elem_expr1))
            }

            (Value::FormatRecord(labels0, formats0), Value::FormatRecord(labels1, formats1))
            | (Value::FormatOverlap(labels0, formats0), Value::FormatOverlap(labels1, formats1)) => {
                labels0 == labels1 && self.is_equal_telescopes(formats0, formats1)
            }

            (Value::ConstLit(const0), Value::ConstLit(const1)) => const0 == const1,

            (_, _) => false,
        }
    }

    /// Check that two [closures][Closure] are equal.
    pub fn is_equal_closures(&mut self, closure0: &Closure<'_>, closure1: &Closure<'_>) -> bool {
        let var = Arc::new(Value::rigid_var(self.rigid_exprs.next_global()));
        let value0 = self.elim_context().apply_closure(closure0, var.clone());
        let value1 = self.elim_context().apply_closure(closure1, var);

        self.push_rigid();
        let result = self.is_equal(&value0, &value1);
        self.pop_rigid();

        result
    }

    /// Check that two [telescopes][Telescope] are equal.
    pub fn is_equal_telescopes(
        &mut self,
        telescope0: &Telescope<'_>,
        telescope1: &Telescope<'_>,
    ) -> bool {
        if telescope0.len() != telescope1.len() {
            return false;
        }

        let initial_rigid_len = self.rigid_exprs;
        let mut telescope0 = telescope0.clone();
        let mut telescope1 = telescope1.clone();

        while let Some(((value0, next_telescope0), (value1, next_telescope1))) = Option::zip(
            self.elim_context().split_telescope(telescope0),
            self.elim_context().split_telescope(telescope1),
        ) {
            if !self.is_equal(&value0, &value1) {
                self.rigid_exprs.truncate(initial_rigid_len);
                return false;
            }

            let var = Arc::new(Value::rigid_var(self.rigid_exprs.next_global()));
            telescope0 = next_telescope0(var.clone());
            telescope1 = next_telescope1(var);
            self.rigid_exprs.push();
        }

        self.rigid_exprs.truncate(initial_rigid_len);
        true
    }

    /// Check that two [constant branches][Branches] are equal.
    fn is_equal_branches<P: PartialEq + Copy>(
        &mut self,
        branches0: &Branches<'_, P>,
        branches1: &Branches<'_, P>,
    ) -> bool {
        use SplitBranches::*;

        let mut branches0 = branches0.clone();
        let mut branches1 = branches1.clone();

        loop {
            match (
                self.elim_context().split_branches(branches0),
                self.elim_context().split_branches(branches1),
            ) {
                (
                    Branch((const0, output_expr0), next_branches0),
                    Branch((const1, output_expr1), next_branches1),
                ) if const0 == const1 && self.is_equal(&output_expr0, &output_expr1) => {
                    branches0 = next_branches0;
                    branches1 = next_branches1;
                }
                (Default(default_expr0), Default(default_expr1)) => {
                    return self.is_equal_closures(&default_expr0, &default_expr1);
                }
                (None, None) => return true,
                (_, _) => return false,
            }
        }
    }

    /// Check that a function literal is equal to a value, using eta-conversion.
    ///
    /// ```fathom
    /// (fun x => f x) = f
    /// ```
    fn is_equal_fun_lit(&mut self, output_expr: &Closure<'_>, value: &ArcValue<'_>) -> bool {
        let var = Arc::new(Value::rigid_var(self.rigid_exprs.next_global()));
        let value = self.elim_context().fun_app(value.clone(), var.clone());
        let output_expr = self.elim_context().apply_closure(output_expr, var);

        self.push_rigid();
        let result = self.is_equal(&output_expr, &value);
        self.pop_rigid();

        result
    }

    /// Check that a record literal is equal to a value, using eta-conversion.
    ///
    /// ```fathom
    /// { x = r.x, y = r.y, .. } = r
    /// ```
    fn is_equal_record_lit(
        &mut self,
        labels: &[StringId],
        exprs: &[ArcValue<'_>],
        value: &ArcValue<'_>,
    ) -> bool {
        Iterator::zip(labels.iter(), exprs.iter()).all(|(label, expr)| {
            let field_value = self.elim_context().record_proj(value.clone(), *label);
            self.is_equal(expr, &field_value)
        })
    }
}
