test : Int =
    match 23 : Int { //~ error: non-exhaustive patterns
        23 => 42,
    };
