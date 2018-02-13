//! The type checking context and binders

use name::{Name, Named};
use syntax::ast::{RcKind, RcType};
use var::{BoundVar, ScopeIndex};

#[derive(Debug, Clone, PartialEq)]
pub enum Scope {
    ExprLam(Vec<Named<Name, RcType>>),
    TypeLam(Vec<Named<Name, RcKind>>),
    TypeDef(Vec<Named<Name, (RcType, RcKind)>>),
}

#[derive(Debug, Clone)]
pub struct Context {
    scopes: Vec<Scope>,
}

impl Context {
    pub fn new() -> Context {
        Context { scopes: Vec::new() }
    }

    pub fn extend(&mut self, scope: Scope) {
        self.scopes.push(scope);
    }

    pub fn lookup(&self, scope: ScopeIndex) -> &Scope {
        assert!(
            self.scopes.len() > scope.0 as usize,
            "ICE: Scope out of range"
        );

        &self.scopes[(self.scopes.len() - scope.0 as usize) - 1]
    }

    pub fn lookup_ty(&self, var: BoundVar) -> Result<(&Name, &RcType), &Scope> {
        match *self.lookup(var.scope) {
            Scope::ExprLam(ref tys) => Ok(
                tys.get(var.binding.0 as usize)
                    .map(|named| (&named.0, &named.1))
                    .expect("ICE: Binder out of range"),
            ),
            ref scope => Err(scope),
        }
    }

    pub fn lookup_ty_def(&self, var: BoundVar) -> Result<(&Name, &RcType), &Scope> {
        match *self.lookup(var.scope) {
            Scope::TypeDef(ref defs) => Ok(
                defs.get(var.binding.0 as usize)
                    .map(|named| (&named.0, &(named.1).0))
                    .expect("ICE: Binder out of range"),
            ),
            ref scope => Err(scope),
        }
    }

    pub fn lookup_kind(&self, var: BoundVar) -> Result<(&Name, &RcKind), &Scope> {
        match *self.lookup(var.scope) {
            Scope::TypeLam(ref kinds) => Ok(
                kinds
                    .get(var.binding.0 as usize)
                    .map(|named| (&named.0, &named.1))
                    .expect("ICE: Binder out of range"),
            ),
            Scope::TypeDef(ref defs) => Ok(
                defs.get(var.binding.0 as usize)
                    .map(|named| (&named.0, &(named.1).1))
                    .expect("ICE: Binder out of range"),
            ),
            ref scope => Err(scope),
        }
    }
}