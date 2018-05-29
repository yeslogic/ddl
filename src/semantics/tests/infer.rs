use super::*;

#[test]
fn free() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let given_expr = r"x";
    let x = Name::user("x");

    assert_eq!(
        infer(&context, &parse(&mut codemap, given_expr)),
        Err(TypeError::UndefinedName {
            var_span: ByteSpan::new(ByteIndex(1), ByteIndex(2)),
            name: x,
        }),
    );
}

#[test]
fn ty() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"Type 1";
    let given_expr = r"Type";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn ty_levels() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"Type 1";
    let given_expr = r"Type 0 : Type 1 : Type 2 : Type 3"; //... Type ∞       ...+:｡(ﾉ･ω･)ﾉﾞ

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn ann_ty_id() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"Type -> Type";
    let given_expr = r"(\a => a) : Type -> Type";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn ann_arrow_ty_id() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"(Type -> Type) -> (Type -> Type)";
    let given_expr = r"(\a => a) : (Type -> Type) -> (Type -> Type)";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn ann_id_as_ty() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let given_expr = r"(\a => a) : Type";

    match infer(&context, &parse(&mut codemap, given_expr)) {
        Err(TypeError::UnexpectedFunction { .. }) => {},
        other => panic!("unexpected result: {:#?}", other),
    }
}

#[test]
fn app() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"Type 1";
    let given_expr = r"(\a : Type 1 => a) Type";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn app_ty() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let given_expr = r"Type Type";

    assert_eq!(
        infer(&context, &parse(&mut codemap, given_expr)),
        Err(TypeError::ArgAppliedToNonFunction {
            fn_span: ByteSpan::new(ByteIndex(1), ByteIndex(5)),
            arg_span: ByteSpan::new(ByteIndex(6), ByteIndex(10)),
            found: Box::new(concrete::Term::Universe(ByteSpan::default(), Some(1))),
        }),
    )
}

#[test]
fn lam() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"(a : Type) -> Type";
    let given_expr = r"\a : Type => a";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn pi() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"Type 1";
    let given_expr = r"(a : Type) -> a";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn id() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"(a : Type) -> a -> a";
    let given_expr = r"\(a : Type) (x : a) => x";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn id_ann() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"(a : Type) -> a -> a";
    let given_expr = r"(\a (x : a) => x) : (A : Type) -> A -> A";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

// Passing `Type` to the polymorphic identity function should yeild the type
// identity function
#[test]
fn id_app_ty() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"Type -> Type";
    let given_expr = r"(\(a : Type 1) (x : a) => x) Type";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

// Passing `Type` to the `Type` identity function should yeild `Type`
#[test]
fn id_app_ty_ty() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"Type 1";
    let given_expr = r"(\(a : Type 2) (x : a) => x) (Type 1) Type";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn id_app_ty_arr_ty() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"Type 1";
    let given_expr = r"(\(a : Type 2) (x : a) => x) (Type 1) (Type -> Type)";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn id_app_arr_pi_ty() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"Type -> Type";
    let given_expr = r"(\(a : Type 1) (x : a) => x) (Type -> Type) (\x => x)";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn apply() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"(a b : Type) -> (a -> b) -> a -> b";
    let given_expr = r"\(a b : Type) (f : a -> b) (x : a) => f x";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn const_() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"(a b : Type) -> a -> b -> a";
    let given_expr = r"\(a b : Type) (x : a) (y : b) => x";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn const_flipped() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"(a b : Type) -> a -> b -> b";
    let given_expr = r"\(a b : Type) (x : a) (y : b) => y";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn flip() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"(a b c : Type) -> (a -> b -> c) -> (b -> a -> c)";
    let given_expr = r"\(a b c : Type) (f : a -> b -> c) (y : b) (x : a) => f x y";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn compose() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"(a b c : Type) -> (b -> c) -> (a -> b) -> (a -> c)";
    let given_expr = r"\(a b c : Type) (f : b -> c) (g : a -> b) (x : a) => f (g x)";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

mod church_encodings {
    use super::*;

    #[test]
    fn and() {
        let mut codemap = CodeMap::new();
        let context = Context::new();

        let expected_ty = r"Type -> Type -> Type 1";
        let given_expr = r"\(p q : Type) => (c : Type) -> (p -> q -> c) -> c";

        assert_term_eq!(
            parse_infer(&mut codemap, &context, given_expr).1,
            parse_normalize(&mut codemap, &context, expected_ty),
        );
    }

    #[test]
    fn and_intro() {
        let mut codemap = CodeMap::new();
        let context = Context::new();

        let expected_ty = r"
            (p q : Type) -> p -> q ->
                ((c : Type) -> (p -> q -> c) -> c)
        ";
        let given_expr = r"
            \(p q : Type) (x : p) (y : q) =>
                \c : Type => \f : (p -> q -> c) => f x y
        ";

        assert_term_eq!(
            parse_infer(&mut codemap, &context, given_expr).1,
            parse_normalize(&mut codemap, &context, expected_ty),
        );
    }

    #[test]
    fn and_proj_left() {
        let mut codemap = CodeMap::new();
        let context = Context::new();

        let expected_ty = r"
            (p q : Type) ->
                ((c : Type) -> (p -> q -> c) -> c) -> p
        ";
        let given_expr = r"
            \(p q : Type) (pq : (c : Type) -> (p -> q -> c) -> c) =>
                pq p (\x y => x)
        ";

        assert_term_eq!(
            parse_infer(&mut codemap, &context, given_expr).1,
            parse_normalize(&mut codemap, &context, expected_ty),
        );
    }

    #[test]
    fn and_proj_right() {
        let mut codemap = CodeMap::new();
        let context = Context::new();

        let expected_ty = r"
            (p q : Type) -> ((c : Type) -> (p -> q -> c) -> c) -> q
        ";
        let given_expr = r"
            \(p q : Type) (pq : (c : Type) -> (p -> q -> c) -> c) =>
                pq q (\x y => y)
        ";

        assert_term_eq!(
            parse_infer(&mut codemap, &context, given_expr).1,
            parse_normalize(&mut codemap, &context, expected_ty),
        );
    }
}

#[test]
fn empty_record_ty() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"Type";
    let given_expr = r"Record {}";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn empty_record() {
    let mut codemap = CodeMap::new();
    let context = Context::new();

    let expected_ty = r"Record {}";
    let given_expr = r"record {}";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn dependent_record_ty() {
    let mut codemap = CodeMap::new();
    let context = Context::default();

    let expected_ty = r"Type 2";
    let given_expr = r"Record { t : Type 1, x : t }";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn record() {
    let mut codemap = CodeMap::new();
    let context = Context::default();

    let given_expr = r#"record { x = "Hello" }"#;

    match infer(&context, &parse(&mut codemap, given_expr)) {
        Err(TypeError::AmbiguousRecord { .. }) => {},
        x => panic!("expected an ambiguous record error, found {:?}", x),
    }
}

#[test]
fn proj() {
    let mut codemap = CodeMap::new();
    let context = Context::default();

    let expected_ty = r"String";
    let given_expr = r#"(record { t = String, x = "hello" } : Record { t : Type, x : String }).x"#;

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn proj_missing() {
    let mut codemap = CodeMap::new();
    let context = Context::default();

    let given_expr = r#"(record { x = "hello" } : Record { x : String }).bloop"#;

    match infer(&context, &parse(&mut codemap, given_expr)) {
        Err(TypeError::NoFieldInType { .. }) => {},
        x => panic!("expected a field lookup error, found {:?}", x),
    }
}

#[test]
fn proj_weird() {
    let mut codemap = CodeMap::new();
    let context = Context::default();

    let expected_ty = r"Type 1";
    let given_expr = r"Record {
        Array : U16 -> Type -> Type,
        t : Record { n : U16, x : Array n I8, y : Array n I8 },
        inner-prod : (len : U16) -> Array len I8 -> Array len I8 -> I32,

        test1 : I32 -> Type,
        test2 : test1 (inner-prod t.n t.x t.y),
    }";

    assert_term_eq!(
        parse_infer(&mut codemap, &context, given_expr).1,
        parse_normalize(&mut codemap, &context, expected_ty),
    );
}

#[test]
fn array_ambiguous() {
    let mut codemap = CodeMap::new();
    let context = Context::default();

    let given_expr = r#"[1, 2 : I32]"#;

    match infer(&context, &parse(&mut codemap, given_expr)) {
        Err(TypeError::AmbiguousArrayLiteral { .. }) => {},
        Err(err) => panic!("unexpected error: {:?}", err),
        Ok((term, ty)) => panic!("expected error, found {} : {}", term, ty),
    }
}
