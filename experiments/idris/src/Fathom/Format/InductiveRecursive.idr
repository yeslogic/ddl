||| A closed universe of format descriptions, using induction recursion between
||| the descriptions and their in-memory representation. This closely matches
||| the current implementation of format descriptions in Fathom.
|||
||| [Induction recursion](https://en.wikipedia.org/wiki/Induction-recursion) is
||| where an inductive datatype is defined simultaneously with a function that
||| operates on that type (see the @Format and @Rep definitions below).
|||
||| The universe is ‘closed’ in the sense tha new format descriptions cannot be
||| added to the type theory, although they can be composed out of other formats)
|||
||| This is similar to the approach used when defining type theories with
||| Tarski-style universes. In-fact inductive-recursive datatypes as a language
||| feature were apparently originally motivated by this use case (see: [“A
||| General Formulation of Simultaneous Inductive-Recursive Definitions in Type
||| Theory”](https://citeseerx.ist.psu.edu/viewdoc/summary?doi=10.1.1.6.4575) by
||| Dybjer).
|||
||| Inspiration for this approach is taken from [“The Power of Pi”](https://cs.ru.nl/~wouters/Publications/ThePowerOfPi.pdf)
||| by Oury and Swierstra.

module Fathom.Format.InductiveRecursive


import Data.Colist
import Data.DPair
import Data.HVect
import Data.Vect

import Fathom.Base
import Fathom.Data.Iso
import Fathom.Data.Sing


-------------------------
-- FORMAT DESCRIPTIONS --
-------------------------


mutual
  ||| Universe of format descriptions
  public export
  data Format : Type where
    End : Format
    Fail : Format
    Pure : {0 A : Type} -> A -> Format
    Ignore : (f : Format) -> (def : f.Rep) -> Format
    Choice : Format -> Format -> Format
    Repeat : Nat -> Format -> Format
    Tuple : {0 len : Nat} -> Vect len Format -> Format
    Pair : Format -> Format -> Format
    Bind : (f : Format) -> (f.Rep -> Format) -> Format


  ||| The in-memory representation of format descriptions
  public export
  Rep : Format -> Type
  Rep End = Unit
  Rep Fail = Void
  Rep (Pure x) = Sing x
  Rep (Ignore _ _) = Unit
  Rep (Choice f1 f2) = Either f1.Rep f2.Rep
  Rep (Repeat len f) = Vect len f.Rep
  Rep (Tuple fs) = HVect (map (.Rep) fs)
  Rep (Pair f1 f2) = (f1.Rep, f2.Rep)
  Rep (Bind f1 f2) = (x : f1.Rep ** (f2 x).Rep)


  ||| Field syntax for representations
  public export
  (.Rep) : Format -> Type
  (.Rep) = Rep


namespace Format

  -- Support for do notation

  public export
  pure : {0 A : Type} -> A -> Format
  pure = Pure

  public export
  (>>=) : (f : Format) -> (f.Rep -> Format) -> Format
  (>>=) = Bind


  ---------------------------
  -- ENCODER/DECODER PAIRS --
  ---------------------------


  export
  decode : (f : Format) -> DecodePart f.Rep (Colist a)
  decode End =
    \case [] => Just ((), [])
          (_::_) => Nothing
  decode Fail = const Nothing
  decode (Pure x) = pure (MkSing x)
  decode (Ignore f _) = ignore (decode f)
  decode (Choice f1 f2) =
    [| Left (decode f1) |] <|> [| Right (decode f2) |]
  decode (Repeat 0 f) = pure []
  decode (Repeat (S len) f) =
    [| decode f :: decode (Repeat len f) |]
  decode (Tuple []) = pure []
  decode (Tuple (f :: fs)) =
    [| decode f :: decode (Tuple fs) |]
  decode (Pair f1 f2) =
    [| (,) (decode f1) (decode f2) |]
  decode (Bind f1 f2) = do
    x <- decode f1
    y <- decode (f2 x)
    pure (x ** y)


  export
  encode : (f : Format) -> Encode f.Rep (Colist a)
  encode End () = pure []
  encode (Pure x) (MkSing _) = pure []
  encode (Ignore f def) () = encode f def
  encode (Choice f1 f2) (Left x) = encode f1 x
  encode (Choice f1 f2) (Right y) = encode f2 y
  encode (Repeat Z f) [] = pure []
  encode (Repeat (S len) f) (x :: xs) =
    [| encode f x <+> encode (Repeat len f) xs |]
  encode (Tuple []) [] = pure []
  encode (Tuple (f :: fs)) (x :: xs) =
    [| encode f x <+> encode (Tuple fs) xs |]
  encode (Pair f1 f2) (x, y) =
    [| encode f1 x <+> encode f2 y |]
  encode (Bind f1 f2) (x ** y) =
    [| encode f1 x <+> encode (f2 x) y |]


---------------------------------
-- INDEXED FORMAT DESCRIPTIONS --
---------------------------------


||| A format description refined with a fixed representation
public export
data FormatOf : (A : Type) -> Type where
  MkFormatOf : (f : Format) -> FormatOf f.Rep


namespace FormatOf

  decode : {0 A : Type} -> (f : FormatOf A) -> Decode (A, ByteStream) ByteStream
  decode  (MkFormatOf f) = Format.decode f


  encode : {0 A : Type} -> (f : FormatOf A) -> Encode A ByteStream
  encode  (MkFormatOf f) = Format.encode f


------------------------------------
-- FORMAT DESCRIPTION CONVERSIONS --
------------------------------------


namespace Format

  public export
  toFormatOf : (f : Format) -> FormatOf f.Rep
  toFormatOf f = MkFormatOf f


  ||| Convert a format description into an indexed format description with an
  ||| equality proof that the representation is the same as the index.
  public export
  toFormatOfEq : {0 A : Type} -> (Subset Format (\f => f.Rep = A)) -> FormatOf A
  toFormatOfEq (Element f prf) = rewrite sym prf in MkFormatOf f


namespace FormatOf

  public export
  toFormat : {0 A : Type} -> FormatOf A -> Format
  toFormat (MkFormatOf f) = f


  ||| Convert an indexed format description to a existential format description,
  ||| along with a proof that the representation is the same as the index.
  public export
  toFormatEq : {0 A : Type} -> FormatOf A -> (Subset Format (\f => f.Rep = A))
  toFormatEq (MkFormatOf f) = Element f Refl


public export
toFormatOfIso : Iso Format (Exists FormatOf)
toFormatOfIso = MkIso
  { to = \f => Evidence _ (toFormatOf f)
  , from = \(Evidence _ f) => toFormat f
  , toFrom = \(Evidence _ (MkFormatOf _)) => Refl
  , fromTo = \_ => Refl
  }


public export
toFormatOfEqIso : Iso (Exists (\a => (Subset Format (\f => f.Rep = a)))) (Exists FormatOf)
toFormatOfEqIso = MkIso
  { to = \(Evidence _ f) => Evidence _ (toFormatOfEq f)
  , from = \(Evidence _ f) => Evidence _ (toFormatEq f)
  , toFrom = \(Evidence _ (MkFormatOf _)) => Refl
  , fromTo = \(Evidence _ (Element _ Refl)) => Refl
  }


---------------------------------
-- INDEXED FORMAT CONSTRUCTORS --
---------------------------------

-- Helpful constructors for building index format descriptions.
-- This also tests if we can actually meaningfully use the `FormatOf` type.

namespace FormatOf

  public export
  end : FormatOf Unit
  end = MkFormatOf End


  public export
  fail : FormatOf Void
  fail = MkFormatOf Fail


  public export
  pure : {0 A : Type} -> (x : A) -> FormatOf (Sing x)
  pure x = MkFormatOf (Pure x)


  public export
  ignore : {0 A : Type} -> (f : FormatOf A) -> (def : A) -> FormatOf Unit
  ignore f def with (toFormatEq f)
    ignore _ def | (Element f prf) = MkFormatOf (Ignore f (rewrite prf in def))


  public export
  repeat : {0 A : Type} -> (len : Nat) -> FormatOf A -> FormatOf (Vect len A)
  repeat len f with (toFormatEq f)
    repeat len _ | (Element f prf) =
      toFormatOfEq (Element (Repeat len f) (cong (Vect len) prf))


  public export
  pair : {0 A, B : Type} -> FormatOf A -> FormatOf B -> FormatOf (A, B)
  pair f1 f2 with (toFormatEq f1, toFormatEq f2)
    pair _ _ | (Element f1 prf1, Element f2 prf2) =
      toFormatOfEq (Element (Pair f1 f2)
        (rewrite prf1 in rewrite prf2 in Refl))


  public export
  bind : {0 A : Type} -> {0 B : A -> Type} -> (f : FormatOf A) -> ((x : A) -> FormatOf (B x)) -> FormatOf (x : A ** B x)
  bind f1 f2 with (toFormatEq f1)
    bind _ f2 | (Element f1 prf) =
      ?todoFormatOf_bind
    --   toFormatOfEq
    --     (Bind f1' (\x =>
    --       case toFormatEq (f2 x) of
    --         (f2' ** prf) => toFormatEq ?todo)
    --     ** rewrite prf in ?todoPrfF1)
    -- -- MkFormatOf (Bind f1 (\x =>
    -- --   case toFormatEq (f2 x) of
    -- --     (f2' ** prf) => toFormatOfEq ?todo))


  public export
  (>>=) : {0 A : Type} -> {0 B : A -> Type} -> (f : FormatOf A) -> ((x : A) -> FormatOf (B x)) -> FormatOf (x : A ** B x)
  (>>=) = bind
