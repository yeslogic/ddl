//! Environments and variables.
//!
//! # Variables
//!
//! Nameless variables are used, which makes alpha equivalence and variable
//! lookup faster.
//!
//! # Environments
//!
//! A number of different environment representations are used - where possible
//! we try to stick to flat, low-indirection environments like [`UniqueEnv`]
//! and [`SliceEnv`], but when we need to copy environments often, we use a
//! [`SharedEnv`] to increase the amount of sharing at the expense of locality.

/// Underlying variable representation.
type RawVar = u16;

/// A [de Bruijn index] in the current environment.
///
/// De Bruijn indices describe an occurrence of a variable in terms of the
/// number of binders between the occurrence and its associated binder.
/// For example:
///
/// | Representation    | Example (S combinator)  |
/// | ----------------- | ----------------------- |
/// | Named             | `λx. λy. λz. x z (y z)` |
/// | De Bruijn indices | `λ_. λ_. λ_. 2 0 (1 0)` |
///
/// This is a helpful representation because it allows us to easily compare
/// terms for equivalence based on their binding structure without maintaining a
/// list of name substitutions. For example we want `λx. x` to be the same as
/// `λy. y`. With de Bruijn indices these would both be described as `λ 0`.
///
/// [de Bruijn index]: https://en.wikipedia.org/wiki/De_Bruijn_index
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalVar(RawVar);

impl LocalVar {
    /// Returns the previously bound variable, relative to this one.
    pub fn prev(self) -> LocalVar {
        LocalVar(self.0 + 1) // FIXME: check overflow?
    }
}

/// An iterator over local variables, listed from the most recently bound.
pub fn local_vars() -> impl Iterator<Item = LocalVar> {
    (0..).map(LocalVar)
}

/// A de Bruijn level in the current environment.
///
/// This describes an occurrence of a variable by counting the binders inwards
/// from the top of the term until the occurrence is reached. For example:
///
/// | Representation    | Example (S combinator)  |
/// | ----------------- | ----------------------- |
/// | Named             | `λx. λy. λz. x z (y z)` |
/// | De Bruijn levels  | `λ_. λ_. λ_. 0 2 (1 2)` |
///
/// Levels are used in [values][crate::core::semantics::Value] because they
/// are not context- dependent (this is in contrast to [indices][LocalVar]).
/// Because of this, we're able to sidestep the need for expensive variable
/// shifting in the semantics. More information can be found in Soham
/// Chowdhury's blog post, “[Real-world type theory I: untyped normalisation by
/// evaluation for λ-calculus][untyped-nbe-for-lc]”.
///
/// [untyped-nbe-for-lc]: https://colimit.net/posts/normalisation-by-evaluation/
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GlobalVar(RawVar);

impl GlobalVar {
    /// Returns the next bound variable, relative to this one.
    pub fn next(self) -> GlobalVar {
        GlobalVar(self.0 + 1) // FIXME: check overflow?
    }
}

/// An iterator over global variables, listed from the least recently bound.
pub fn global_vars() -> impl Iterator<Item = GlobalVar> {
    (0..).map(GlobalVar)
}

/// The length of an environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnvLen(RawVar);

impl EnvLen {
    /// Construct a new, empty environment.
    pub fn new() -> EnvLen {
        EnvLen(0)
    }

    pub fn clear(&mut self) {
        *self = EnvLen::new();
    }

    pub fn local_to_global(self, local: LocalVar) -> Option<GlobalVar> {
        Some(GlobalVar(self.0.checked_sub(local.0)?.checked_sub(1)?))
    }

    pub fn global_to_local(self, global: GlobalVar) -> Option<LocalVar> {
        Some(LocalVar(self.0.checked_sub(global.0)?.checked_sub(1)?))
    }

    pub fn next_global(self) -> GlobalVar {
        GlobalVar(self.0)
    }

    pub fn push(&mut self) {
        self.0 += 1; // FIXME: check overflow?
    }

    pub fn pop(&mut self) {
        self.0 -= 1; // FIXME: check underflow?
    }
}

/// A uniquely owned environment.
#[derive(Debug, Clone)]
pub struct UniqueEnv<Entry> {
    entries: Vec<Entry>,
}

impl<Entry> UniqueEnv<Entry> {
    /// Construct a new, empty environment.
    pub fn new() -> UniqueEnv<Entry> {
        UniqueEnv {
            entries: Vec::new(),
        }
    }

    /// Clear the renaming. This is useful for reusing environment allocations.
    pub fn clear(&mut self) {
        self.entries.clear()
    }

    /// Resize the environment to the desired length, filling new entries with `entry`.
    pub fn resize(&mut self, new_len: EnvLen, entry: Entry)
    where
        Entry: Clone,
    {
        self.entries.resize(usize::from(new_len.0), entry)
    }

    /// Push an entry onto the environment.
    pub fn push(&mut self, entry: Entry) {
        assert!(self.entries.len() < usize::from(u16::MAX));
        self.entries.push(entry);
    }

    /// Pop an entry off the environment.
    pub fn pop(&mut self) {
        self.entries.pop();
    }
}

impl<Entry> std::ops::Deref for UniqueEnv<Entry> {
    type Target = SliceEnv<Entry>;

    fn deref(&self) -> &SliceEnv<Entry> {
        // SAFETY:
        // - `SliceEnv<Entry>` is equivalent to a `[Entry]` internally
        unsafe { std::mem::transmute::<&[_], &SliceEnv<_>>(&self.entries[..]) }
    }
}

impl<Entry> std::ops::DerefMut for UniqueEnv<Entry> {
    fn deref_mut(&mut self) -> &mut SliceEnv<Entry> {
        // SAFETY:
        // - `SliceEnv<Entry>` is equivalent to a `[Entry]` internally
        unsafe { std::mem::transmute::<&mut [_], &mut SliceEnv<_>>(&mut self.entries[..]) }
    }
}

/// An environment backed by a slice.
#[derive(Debug)]
pub struct SliceEnv<Entry> {
    entries: [Entry],
}

impl<Entry> SliceEnv<Entry> {
    /// The length of the environment.
    pub fn len(&self) -> EnvLen {
        EnvLen(self.entries.len() as RawVar)
    }

    /// Lookup an entry in the environment using global variable reference.
    pub fn get_global(&self, global_var: GlobalVar) -> Option<&Entry> {
        self.entries.get(usize::from(global_var.0))
    }

    /// Lookup an entry in the environment using a local variable reference.
    pub fn get_local(&self, local_var: LocalVar) -> Option<&Entry> {
        self.get_global(self.len().local_to_global(local_var)?)
    }

    /// Set an entry in the environment using global variable reference.
    pub fn set_global(&mut self, global_var: GlobalVar, entry: Entry) {
        self.entries[usize::from(global_var.0)] = entry;
    }

    /// Iterate over the elements in the environment.
    pub fn iter<'this>(&'this self) -> impl 'this + DoubleEndedIterator<Item = &'this Entry> {
        self.entries.iter()
    }
}

/// A persistent environment with structural sharing.
#[derive(Debug, Clone)]
pub struct SharedEnv<Entry> {
    // An `rpds::Vector` is used instead of an `im::Vector` as it's a bit
    // more compact. We assume this is important because we tend to clone
    // environments often, and they contribute to the overall size of values.
    //
    // TODO: validate these assumptions by benchmarking against other internal
    //       representations, for example:
    //
    // - `Vec<_>`
    // - `Arc<Vec<_>>`
    // - `im::Vector<_>`
    // - `Arc<im::Vector<_>>`
    entries: rpds::VectorSync<Entry>,
}

impl<Entry> SharedEnv<Entry> {
    /// Construct a new, empty environment.
    pub fn new() -> SharedEnv<Entry> {
        SharedEnv {
            entries: rpds::Vector::new_sync(),
        }
    }

    /// The length of the environment.
    pub fn len(&self) -> EnvLen {
        EnvLen(self.entries.len() as u16)
    }

    /// Lookup an entry in the environment using global variable reference.
    pub fn get_global(&self, global_var: GlobalVar) -> Option<&Entry> {
        self.entries.get(usize::from(global_var.0))
    }

    /// Lookup an entry in the environment using a local variable reference.
    pub fn get_local(&self, local_var: LocalVar) -> Option<&Entry> {
        self.get_global(self.len().local_to_global(local_var)?)
    }

    /// Push an entry onto a clone of the environment.
    pub fn push_clone(&self, entry: Entry) -> SharedEnv<Entry> {
        assert!(self.entries.len() < usize::from(u16::MAX));
        SharedEnv {
            entries: self.entries.push_back(entry),
        }
    }

    /// Push an entry onto the environment.
    pub fn push(&mut self, entry: Entry) {
        assert!(self.entries.len() < usize::from(u16::MAX));
        self.entries.push_back_mut(entry);
    }

    /// Pop an entry off the environment.
    pub fn pop(&mut self) {
        self.entries.drop_last_mut();
    }

    /// Iterate over the elements in the environment.
    pub fn iter<'this>(&'this self) -> impl 'this + DoubleEndedIterator<Item = &'this Entry> {
        self.entries.iter()
    }
}
