extern U8 : Format;

struct Pair {
    first: U8,
    second: U8,
    first: U8, //~ error: field `first` is already declared
    first: U8, //~ error: field `first` is already declared
    second: U8, //~ error: field `second` is already declared
}
