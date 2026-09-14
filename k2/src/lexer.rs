//! The K2 lexer: source text → tokens with spans.
//!
//! C#-shaped: `//`/`/* */` comments, `///` doc comments, `$"…{expr}…"`
//! interpolation, `_` digit separators, and the operator set from
//! `design/k2/spec.md` §2.

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    // literals
    Int(i128, IntSuffix),
    Float(f64, bool /* is f32 */),
    Str(String),
    /// An interpolated string, already split into literal chunks and holes.
    /// `parts` alternates: literal, hole-source, literal, hole-source, … The
    /// parser re-lexes each hole. `Even` indices are literals.
    InterpStr(Vec<InterpPart>),
    Char(char),
    Ident(String),
    Keyword(Kw),

    // punctuation & operators
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Semi,
    Comma,
    Dot,
    DotDot,   // ..
    DotDotLt, // ..<
    Colon,
    ColonColon,
    Question,
    QuestionQuestion, // ??
    QuestionDot,      // ?.
    QuestionQEq,      // ??=
    Arrow,            // ->
    FatArrow,         // =>
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl, // <<
    Shr, // >>
    UShr, // >>> — shifts zeroes in, whatever the sign
    AmpAmp,
    PipePipe,
    Bang,
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    Ne,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    PlusPlus,
    MinusMinus,
    Bang2, // reserved

    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InterpPart {
    Lit(String),
    /// Raw source of the `{…}` hole (without braces), re-lexed by the parser.
    Hole(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntSuffix {
    None,
    U,
    L,
    UL,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kw {
    Namespace,
    Using,
    Public,
    Private,
    Internal,
    Static,
    Const,
    Var,
    Let,
    Class,
    Record,
    Struct,
    Enum,
    Interface,
    Form,
    New,
    This,
    Return,
    If,
    Else,
    Switch,
    Case,
    Default,
    For,
    Foreach,
    In,
    While,
    Do,
    Break,
    Continue,
    Defer,
    True,
    False,
    Null,
    Is,
    As,
    Ref,
    Out,
    Void,
}

impl Kw {
    /// Whether a name is a keyword, and so needs `@` to be used as an
    /// identifier.
    pub fn is_keyword(s: &str) -> bool {
        Kw::from_ident(s).is_some()
    }

    fn from_ident(s: &str) -> Option<Kw> {
        Some(match s {
            "namespace" => Kw::Namespace,
            "using" => Kw::Using,
            "public" => Kw::Public,
            "private" => Kw::Private,
            "internal" => Kw::Internal,
            "static" => Kw::Static,
            "const" => Kw::Const,
            "var" => Kw::Var,
            "let" => Kw::Let,
            "class" => Kw::Class,
            "record" => Kw::Record,
            "struct" => Kw::Struct,
            "enum" => Kw::Enum,
            "interface" => Kw::Interface,
            "form" => Kw::Form,
            "new" => Kw::New,
            "this" => Kw::This,
            "return" => Kw::Return,
            "if" => Kw::If,
            "else" => Kw::Else,
            "switch" => Kw::Switch,
            "case" => Kw::Case,
            "default" => Kw::Default,
            "for" => Kw::For,
            "foreach" => Kw::Foreach,
            "in" => Kw::In,
            "while" => Kw::While,
            "do" => Kw::Do,
            "break" => Kw::Break,
            "continue" => Kw::Continue,
            "defer" => Kw::Defer,
            "true" => Kw::True,
            "false" => Kw::False,
            "null" => Kw::Null,
            "is" => Kw::Is,
            "as" => Kw::As,
            "ref" => Kw::Ref,
            "out" => Kw::Out,
            "void" => Kw::Void,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spanned {
    pub tok: Tok,
    pub span: Span,
    /// A doc comment (`///`) that immediately preceded this token.
    pub doc: Option<String>,
    /// Ordinary comments that preceded this token, in order, each without its
    /// `//`. They belong to whatever construct starts here, so the printer can
    /// put them back.
    pub leading: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct LexError {
    pub msg: String,
    pub line: usize,
    pub col: usize,
}

struct Lexer<'a> {
    src: &'a [u8],
    i: usize,
    line: usize,
    col: usize,
    pending_doc: Option<String>,
    /// Ordinary comments seen since the last token.
    pending_comments: Vec<String>,
    /// Whether an ordinary (non-doc) comment was seen at all.
    saw_comment: bool,
}

pub fn lex(src: &str) -> Result<Vec<Spanned>, LexError> {
    Ok(lex_with_trivia(src)?.0)
}

/// Lex, also reporting whether the source carries ordinary comments.
pub fn lex_with_trivia(src: &str) -> Result<(Vec<Spanned>, bool), LexError> {
    let mut lx = Lexer {
        src: src.as_bytes(),
        i: 0,
        line: 1,
        col: 1,
        pending_doc: None,
        pending_comments: Vec::new(),
        saw_comment: false,
    };
    let toks = lx.run()?;
    Ok((toks, lx.saw_comment))
}

impl Lexer<'_> {
    fn peek(&self) -> u8 {
        *self.src.get(self.i).unwrap_or(&0)
    }
    fn peek2(&self) -> u8 {
        *self.src.get(self.i + 1).unwrap_or(&0)
    }
    fn peek3(&self) -> u8 {
        *self.src.get(self.i + 2).unwrap_or(&0)
    }
    /// Consume one whole UTF-8 scalar. Source content — a string's characters,
    /// a comment's text — must not be read a byte at a time, or anything
    /// outside ASCII is mangled.
    fn bump_char(&mut self) -> char {
        let rest = &self.src[self.i..];
        let s = std::str::from_utf8(rest).unwrap_or("");
        match s.chars().next() {
            Some(c) => {
                for _ in 0..c.len_utf8() {
                    self.bump();
                }
                c
            }
            None => {
                self.bump();
                '\u{FFFD}'
            }
        }
    }

    fn bump(&mut self) -> u8 {
        let c = self.peek();
        self.i += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        c
    }
    fn err(&self, msg: impl Into<String>) -> LexError {
        LexError {
            msg: msg.into(),
            line: self.line,
            col: self.col,
        }
    }

    fn run(&mut self) -> Result<Vec<Spanned>, LexError> {
        let mut out = Vec::new();
        loop {
            self.skip_trivia()?;
            let line = self.line;
            let col = self.col;
            if self.i >= self.src.len() {
                out.push(self.spanned(Tok::Eof, line, col));
                break;
            }
            let tok = self.next_token()?;
            out.push(self.spanned(tok, line, col));
        }
        Ok(out)
    }

    fn spanned(&mut self, tok: Tok, line: usize, col: usize) -> Spanned {
        Spanned {
            tok,
            span: Span {
                line,
                col,
                end_col: self.col,
            },
            doc: self.pending_doc.take(),
            leading: std::mem::take(&mut self.pending_comments),
        }
    }

    fn skip_trivia(&mut self) -> Result<(), LexError> {
        loop {
            let c = self.peek();
            if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' {
                self.bump();
            } else if c == b'/' && self.peek2() == b'/' {
                // `///` doc vs `//` line comment.
                let is_doc = self.peek3() == b'/';
                self.bump();
                self.bump();
                if is_doc {
                    self.bump();
                    if self.peek() == b' ' {
                        self.bump();
                    }
                }
                let mut text = String::new();
                while self.peek() != b'\n' && self.i < self.src.len() {
                    text.push(self.bump_char());
                }
                if !is_doc {
                    self.saw_comment = true;
                }
                if is_doc {
                    match &mut self.pending_doc {
                        Some(d) => {
                            d.push('\n');
                            d.push_str(&text);
                        }
                        None => self.pending_doc = Some(text),
                    }
                } else {
                    self.pending_comments.push(text);
                }
            } else if c == b'/' && self.peek2() == b'*' {
                self.saw_comment = true;
                self.bump();
                self.bump();
                let mut depth = 1;
                while depth > 0 {
                    if self.i >= self.src.len() {
                        return Err(self.err("unterminated block comment"));
                    }
                    if self.peek() == b'/' && self.peek2() == b'*' {
                        self.bump();
                        self.bump();
                        depth += 1;
                    } else if self.peek() == b'*' && self.peek2() == b'/' {
                        self.bump();
                        self.bump();
                        depth -= 1;
                    } else {
                        self.bump();
                    }
                }
            } else {
                return Ok(());
            }
        }
    }

    fn next_token(&mut self) -> Result<Tok, LexError> {
        let c = self.peek();
        if c == b'_' || c.is_ascii_alphabetic() {
            return Ok(self.ident());
        }
        // `@name` uses a keyword as an ordinary name (spec §2). The `@` is not
        // part of the identity, so `@out` and a non-keyword `out` are one name.
        if c == b'@' {
            self.bump();
            let mut s = String::new();
            while self.peek() == b'_' || self.peek().is_ascii_alphanumeric() {
                s.push(self.bump() as char);
            }
            return Ok(Tok::Ident(s));
        }
        if c.is_ascii_digit() {
            return self.number();
        }
        if c == b'"' {
            return self.string(false);
        }
        if c == b'$' && self.peek2() == b'"' {
            self.bump();
            return self.string(true);
        }
        if c == b'\'' {
            return self.char_lit();
        }
        self.operator()
    }

    fn ident(&mut self) -> Tok {
        let mut s = String::new();
        while self.peek() == b'_' || self.peek().is_ascii_alphanumeric() {
            s.push(self.bump() as char);
        }
        match Kw::from_ident(&s) {
            Some(kw) => Tok::Keyword(kw),
            None => Tok::Ident(s),
        }
    }

    fn number(&mut self) -> Result<Tok, LexError> {
        let mut s = String::new();
        let mut is_float = false;
        let mut radix = 10u32;
        if self.peek() == b'0' && (self.peek2() == b'x' || self.peek2() == b'X') {
            self.bump();
            self.bump();
            radix = 16;
            while self.peek() == b'_' || self.peek().is_ascii_hexdigit() {
                let c = self.bump();
                if c != b'_' {
                    s.push(c as char);
                }
            }
        } else if self.peek() == b'0' && (self.peek2() == b'b' || self.peek2() == b'B') {
            self.bump();
            self.bump();
            radix = 2;
            while self.peek() == b'_' || self.peek() == b'0' || self.peek() == b'1' {
                let c = self.bump();
                if c != b'_' {
                    s.push(c as char);
                }
            }
        } else {
            while self.peek() == b'_' || self.peek().is_ascii_digit() {
                let c = self.bump();
                if c != b'_' {
                    s.push(c as char);
                }
            }
            // fractional / exponent
            if self.peek() == b'.' && self.peek2().is_ascii_digit() {
                is_float = true;
                s.push(self.bump() as char); // .
                while self.peek() == b'_' || self.peek().is_ascii_digit() {
                    let c = self.bump();
                    if c != b'_' {
                        s.push(c as char);
                    }
                }
            }
            if self.peek() == b'e' || self.peek() == b'E' {
                is_float = true;
                s.push(self.bump() as char);
                if self.peek() == b'+' || self.peek() == b'-' {
                    s.push(self.bump() as char);
                }
                while self.peek().is_ascii_digit() {
                    s.push(self.bump() as char);
                }
            }
        }
        // suffixes
        let mut suffix = IntSuffix::None;
        let mut is_f32 = false;
        loop {
            match self.peek() {
                b'f' | b'F' => {
                    self.bump();
                    is_float = true;
                    is_f32 = true;
                    break;
                }
                b'd' | b'D' if radix == 10 => {
                    self.bump();
                    is_float = true;
                    break;
                }
                b'u' | b'U' => {
                    self.bump();
                    suffix = if suffix == IntSuffix::L {
                        IntSuffix::UL
                    } else {
                        IntSuffix::U
                    };
                }
                b'l' | b'L' => {
                    self.bump();
                    suffix = if suffix == IntSuffix::U {
                        IntSuffix::UL
                    } else {
                        IntSuffix::L
                    };
                }
                _ => break,
            }
        }
        if is_float {
            let v: f64 = s
                .parse()
                .map_err(|_| self.err(format!("bad float `{s}`")))?;
            Ok(Tok::Float(v, is_f32))
        } else {
            let v = i128::from_str_radix(&s, radix)
                .map_err(|_| self.err(format!("bad integer `{s}`")))?;
            Ok(Tok::Int(v, suffix))
        }
    }

    fn escape(&mut self) -> Result<char, LexError> {
        // caller has consumed the backslash
        let c = self.bump();
        Ok(match c {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'0' => '\0',
            b'\\' => '\\',
            b'"' => '"',
            b'\'' => '\'',
            b'{' => '{',
            b'}' => '}',
            b'u' => {
                // \u{XXXX}
                if self.bump() != b'{' {
                    return Err(self.err("expected `{` after \\u"));
                }
                let mut hex = String::new();
                while self.peek() != b'}' {
                    hex.push(self.bump() as char);
                }
                self.bump(); // }
                let n =
                    u32::from_str_radix(&hex, 16).map_err(|_| self.err("bad unicode escape"))?;
                char::from_u32(n).ok_or_else(|| self.err("invalid code point"))?
            }
            other => return Err(self.err(format!("bad escape `\\{}`", other as char))),
        })
    }

    fn string(&mut self, interp: bool) -> Result<Tok, LexError> {
        self.bump(); // opening quote
        if !interp {
            let mut s = String::new();
            loop {
                let c = self.peek();
                if c == 0 {
                    return Err(self.err("unterminated string"));
                }
                if c == b'"' {
                    self.bump();
                    break;
                }
                if c == b'\\' {
                    self.bump();
                    s.push(self.escape()?);
                } else {
                    s.push(self.bump_char());
                }
            }
            return Ok(Tok::Str(s));
        }
        // interpolated
        let mut parts = Vec::new();
        let mut cur = String::new();
        loop {
            let c = self.peek();
            if c == 0 {
                return Err(self.err("unterminated interpolated string"));
            }
            if c == b'"' {
                self.bump();
                break;
            }
            if c == b'\\' {
                self.bump();
                cur.push(self.escape()?);
            } else if c == b'{' {
                if self.peek2() == b'{' {
                    self.bump();
                    self.bump();
                    cur.push('{');
                    continue;
                }
                self.bump(); // {
                parts.push(InterpPart::Lit(std::mem::take(&mut cur)));
                let mut hole = String::new();
                let mut depth = 1;
                while depth > 0 {
                    let h = self.peek();
                    if h == 0 {
                        return Err(self.err("unterminated interpolation hole"));
                    }
                    if h == b'{' {
                        depth += 1;
                    } else if h == b'}' {
                        depth -= 1;
                        if depth == 0 {
                            self.bump();
                            break;
                        }
                    }
                    hole.push(self.bump_char());
                }
                parts.push(InterpPart::Hole(hole));
            } else if c == b'}' && self.peek2() == b'}' {
                self.bump();
                self.bump();
                cur.push('}');
            } else {
                cur.push(self.bump_char());
            }
        }
        parts.push(InterpPart::Lit(cur));
        Ok(Tok::InterpStr(parts))
    }

    fn char_lit(&mut self) -> Result<Tok, LexError> {
        self.bump(); // '
        let c = if self.peek() == b'\\' {
            self.bump();
            self.escape()?
        } else {
            self.bump_char()
        };
        if self.bump() != b'\'' {
            return Err(self.err("unterminated char literal"));
        }
        Ok(Tok::Char(c))
    }

    fn operator(&mut self) -> Result<Tok, LexError> {
        let c = self.bump();
        let d = self.peek();
        macro_rules! two {
            ($tok:expr) => {{
                self.bump();
                return Ok($tok);
            }};
        }
        Ok(match c {
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b'{' => Tok::LBrace,
            b'}' => Tok::RBrace,
            b'[' => Tok::LBracket,
            b']' => Tok::RBracket,
            b';' => Tok::Semi,
            b',' => Tok::Comma,
            b'.' => {
                if d == b'.' {
                    self.bump();
                    if self.peek() == b'<' {
                        self.bump();
                        Tok::DotDotLt
                    } else {
                        Tok::DotDot
                    }
                } else {
                    Tok::Dot
                }
            }
            b':' => {
                if d == b':' {
                    two!(Tok::ColonColon)
                } else {
                    Tok::Colon
                }
            }
            b'?' => match d {
                b'?' => {
                    self.bump();
                    if self.peek() == b'=' {
                        self.bump();
                        Tok::QuestionQEq
                    } else {
                        Tok::QuestionQuestion
                    }
                }
                b'.' => two!(Tok::QuestionDot),
                _ => Tok::Question,
            },
            b'+' => match d {
                b'+' => two!(Tok::PlusPlus),
                b'=' => two!(Tok::PlusEq),
                _ => Tok::Plus,
            },
            b'-' => match d {
                b'-' => two!(Tok::MinusMinus),
                b'=' => two!(Tok::MinusEq),
                b'>' => two!(Tok::Arrow),
                _ => Tok::Minus,
            },
            b'*' => {
                if d == b'=' {
                    two!(Tok::StarEq)
                } else {
                    Tok::Star
                }
            }
            b'/' => {
                if d == b'=' {
                    two!(Tok::SlashEq)
                } else {
                    Tok::Slash
                }
            }
            b'%' => {
                if d == b'=' {
                    two!(Tok::PercentEq)
                } else {
                    Tok::Percent
                }
            }
            b'&' => {
                if d == b'&' {
                    two!(Tok::AmpAmp)
                } else {
                    Tok::Amp
                }
            }
            b'|' => {
                if d == b'|' {
                    two!(Tok::PipePipe)
                } else {
                    Tok::Pipe
                }
            }
            b'^' => Tok::Caret,
            b'~' => Tok::Tilde,
            b'<' => match d {
                b'<' => two!(Tok::Shl),
                b'=' => two!(Tok::Le),
                _ => Tok::Lt,
            },
            b'>' => match d {
                // NB: `>>` is split by the parser when closing generics; here
                // we only emit Shr, and the parser handles `>` `>` for types.
                // `>>>`, C#'s unsigned shift, before `>>`.
                b'>' if self.peek2() == b'>' => {
                    self.bump();
                    self.bump();
                    return Ok(Tok::UShr);
                }
                b'>' => two!(Tok::Shr),
                b'=' => two!(Tok::Ge),
                _ => Tok::Gt,
            },
            b'=' => match d {
                b'=' => two!(Tok::EqEq),
                b'>' => two!(Tok::FatArrow),
                _ => Tok::Eq,
            },
            b'!' => {
                if d == b'=' {
                    two!(Tok::Ne)
                } else {
                    Tok::Bang
                }
            }
            other => return Err(self.err(format!("unexpected character `{}`", other as char))),
        })
    }
}
