extern Bool : Type;

extern true : item Bool;

extern F32 : Type;

Foo = f32 33.4 : item F32;

test = bool_elim item true { item true, ! } : item Bool;
