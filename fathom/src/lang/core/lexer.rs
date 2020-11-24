use logos::Logos;
use std::fmt;

use crate::lang::Range;
use crate::reporting::LexerMessage;

/// Tokens in the core language.
#[derive(Debug, Clone, Logos)]
pub enum Token<'source> {
    #[regex(r"///(.*)\n", |lexer| lexer.slice()[3..].trim_end().to_owned())]
    DocComment(String),
    #[regex(r"//!(.*)\n", |lexer| lexer.slice()[3..].trim_end().to_owned())]
    InnerDocComment(String),

    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Name(&'source str),
    #[regex(r#"'([^'\\]|\\.)*'"#)]
    CharLiteral(&'source str),
    #[regex(r#""([^"\\]|\\.)*""#)]
    StringLiteral(&'source str),
    #[regex(r"[-+]?[0-9][a-zA-Z0-9_\.]*")]
    NumericLiteral(&'source str),

    #[token("array")]
    Array,
    #[token("bool_elim")]
    BoolElim,
    #[token("const")]
    Const,
    #[token("f32")]
    F32,
    #[token("f64")]
    F64,
    #[token("Format")]
    Format,
    #[token("global")]
    Global,
    #[token("int")]
    Int,
    #[token("int_elim")]
    IntElim,
    #[token("item")]
    Item,
    #[token("Kind")]
    Kind,
    #[token("local")]
    Local,
    #[token("repr")]
    Repr,
    #[token("struct")]
    Struct,
    #[token("Type")]
    Type,

    #[token("{")]
    OpenBrace,
    #[token("}")]
    CloseBrace,
    #[token("[")]
    OpenBracket,
    #[token("]")]
    CloseBracket,
    #[token("(")]
    OpenParen,
    #[token(")")]
    CloseParen,

    #[token("!")]
    Bang,
    #[token(":")]
    Colon,
    #[token(",")]
    Comma,
    #[token("=")]
    Equals,
    #[token("=>")]
    EqualsGreater,
    #[token(".")]
    FullStop,
    #[token("->")]
    HyphenGreater,
    #[token(";")]
    Semi,

    #[error]
    #[regex(r"\p{Whitespace}", logos::skip)]
    #[regex(r"//(.*)\n", logos::skip)]
    Error,
}

impl<'source> fmt::Display for Token<'source> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::DocComment(source) => write!(f, "{}", source),
            Token::InnerDocComment(source) => write!(f, "{}", source),

            Token::Name(source) => write!(f, "{}", source),
            Token::CharLiteral(source) => write!(f, "{}", source),
            Token::StringLiteral(source) => write!(f, "{}", source),
            Token::NumericLiteral(source) => write!(f, "{}", source),

            Token::Array => write!(f, "array"),
            Token::BoolElim => write!(f, "bool_elim"),
            Token::Const => write!(f, "const"),
            Token::F32 => write!(f, "f32"),
            Token::F64 => write!(f, "f64"),
            Token::Format => write!(f, "Format"),
            Token::Global => write!(f, "global"),
            Token::Int => write!(f, "int"),
            Token::IntElim => write!(f, "int_elim"),
            Token::Item => write!(f, "item"),
            Token::Kind => write!(f, "Kind"),
            Token::Local => write!(f, "local"),
            Token::Repr => write!(f, "repr"),
            Token::Struct => write!(f, "struct"),
            Token::Type => write!(f, "Type"),

            Token::OpenBrace => write!(f, "{{"),
            Token::CloseBrace => write!(f, "}}"),
            Token::OpenBracket => write!(f, "["),
            Token::CloseBracket => write!(f, "]"),
            Token::OpenParen => write!(f, "("),
            Token::CloseParen => write!(f, ")"),

            Token::Bang => write!(f, "!"),
            Token::Colon => write!(f, ":"),
            Token::Comma => write!(f, ","),
            Token::Equals => write!(f, "="),
            Token::EqualsGreater => write!(f, "=>"),
            Token::FullStop => write!(f, "."),
            Token::HyphenGreater => write!(f, "->"),
            Token::Semi => write!(f, ";"),

            Token::Error => write!(f, "<error>"),
        }
    }
}

pub type Spanned<Tok, Loc> = (Loc, Tok, Loc);

pub fn tokens<'source>(
    file_id: usize,
    source: &'source str,
) -> impl 'source + Iterator<Item = Result<Spanned<Token<'source>, usize>, LexerMessage>> {
    Token::lexer(source)
        .spanned()
        .map(move |(token, range)| match token {
            Token::Error => Err(LexerMessage::InvalidToken {
                file_id,
                range: Range::from(range),
            }),
            token => Ok((range.start, token, range.end)),
        })
}
