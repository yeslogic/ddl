
# Roadmap

## Language features

- [x] top-level items
- [ ] recursive definitions
- [x] let expressions
- [x] dependent function types
  - [x] condensed syntax for multiple parameters
  - [x] implicit parameters
- [x] records
  - [x] non-dependent
  - [x] dependent
- [x] holes
  - [x] named holes
- [x] numeric types
- [x] numeric literals
  - [x] decimal literals
  - [ ] non-decimal radixes
  - [ ] custom parsing
- [x] array types
- [x] array literals
- [x] binary format descriptions
  - [x] succcess formats
  - [x] failure formats
  - [ ] end-of-input formats
  - [x] record formats
    - [ ] conditional field sugar
    - [ ] skipped fields
  - [x] position formats
  - [ ] link formats
  - [ ] map formats
  - [x] numeric formats
  - [x] array formats
  - [ ] uniform-choice formats
  - [ ] choice formats
  - [ ] repeat formats
- [ ] refinement types
- [x] match expressions
  - [x] single-layer pattern matching
  - [x] multi-layer pattern matching
  - [ ] dependent pattern matching
- [x] patterns
  - [x] wildcard patterns
  - [x] named patterns
  - [x] annotated patterns
  - [x] numeric literal patterns
  - [x] record literal patterns
- [ ] invertible format descriptions

## Implementation

- [x] command line interface
  - [x] parse arbitrary top-level items
  - [ ] parse from an offset
  - [ ] navigation through links and offsets
  - [ ] serialise data to JSON for use with tools like jq
- [x] parser
- [x] pretty printing
  - [x] surface language
  - [x] core language
- [x] source locations
  - [x] surface language
  - [x] core language
- [x] string interning
- [x] arena allocation
- [ ] value interning (for commonly used values)
- [x] normalisation-by-evaluation
  - [ ] stack traces
- [x] elaborator
  - [x] error recovery
  - [x] unification
  - [x] zonking
- [x] distiller
  - [x] improve binder names
  - [x] improve hole names
- [ ] core language validation
- [x] binary format interpreter
  - [x] parser
  - [ ] pretty printer
- [ ] compiler
- [x] codespan diagnostics
  - [x] unification solutions
  - [x] terms and types included in messages
- [x] integration tests
  - [x] basic error code checks
  - [x] snapshot testing
  - [x] diagnostic expectations
  - [x] binary parser tests

## Ideas for future experimentation
- integer refinements (see [Refinement Types: A tutorial](https://arxiv.org/abs/2010.07763) and [sprite-lang](https://github.com/ranjitjhala/sprite-lang), and also this [twitter thread](https://twitter.com/brendanzab/status/1403528996474609666))
