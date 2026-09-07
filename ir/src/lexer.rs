//! Tokenizer for the Kiln IR text encoding (`.kiln`).
//!
//! Line structure is significant only in that statements end at a newline; the
//! lexer emits explicit `Newline` tokens and the parser consumes them.  Blank
//! lines and `# ...` comments are skipped here.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // Keywords
    Module,
    Sub,
    End,
    Let,
    Var,
    Call,
    If,
    Else,
    While,
    For,
    Break,
    Continue,
    And,
    Or,
    Not,
    /// `bnot` — bitwise complement. Reserved, exactly as `not` is.
    ///
    /// The *infix* bitwise words (`band`, `bor`, `bxor`, `shl`, `shr`, `ushr`)
    /// are soft keywords the parser recognises only in operator position,
    /// where an identifier could never have appeared — so a name spelled like
    /// one keeps working. A prefix operator has no such shelter: `bnot(x)` and
    /// `bnot -1` read as the operator and as a call or a subtraction equally
    /// well, and choosing wrong is a wrong answer rather than an error. So
    /// this one is a keyword, and a name spelled `bnot` is refused where it is
    /// written instead of quietly meaning something else.
    BNot,
    True,
    False,
    Use,
    Form,
    On,
    Return,
    // Literals / identifiers
    Ident(String),
    Int(i64),
    /// A hex or binary literal — `0xFF`, `0b1010`. Kept as the raw **bit
    /// pattern** rather than a signed value, because how wide it is depends on
    /// where it lands: `0x8000_0000` is `int` -2147483648 on its own and
    /// `int64` 2147483648 where an `int64` is wanted, and folding either
    /// reading in here would make the other one unreachable.
    Bits(u64),
    Float(f64),
    Str(String),
    /// A **raw** text literal — `r"C:\\logs"`, or `r"""..."""` for one that
    /// runs over lines. Its bytes are exactly what was written: no `\n`/`\t`
    /// escapes, and no `{...}` interpolation.
    ///
    /// It is a token of its own rather than a flag on `Str` because the two
    /// differ in what the *parser* then does — a `Str` is split into its
    /// interpolation holes and a `RawStr` never is — and a flag would have to be
    /// read at every one of those places to mean anything.
    RawStr(String),
    // Punctuation / operators
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    /// `?` — written after a type, and only there: `text?` is the text that may
    /// not be there. It is a token of its own because nothing else in the
    /// language spells `?`, so a stray one is an error with a position rather
    /// than a character the lexer had to guess at.
    Question,
    /// `..` — the range operator in `for i in 1..10`. A fused token so a range
    /// bound's expression ends cleanly at the `..`; without it `A..B` would lex
    /// as `A` then a `.` that the expression parser reads as a field access into
    /// a second `.`, which is nothing. It is also the range in a slice —
    /// `s[2..5]`, `xs[3..]` — which is the same "from here to there" it means in
    /// a `for`, and reads with the same token.
    DotDot,
    Eq,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    /// The compound-assignment operators `+= -= *= /=` and the text-append
    /// `&=`. They are fused tokens rather than a `+` beside a `=` because the
    /// two only ever mean *assign back into the target*, and lexing them whole
    /// keeps the parser from having to distinguish `x += 1` from a stray `+`
    /// that happens to precede an `=`. `&=` is the only one whose first
    /// character (`&`) is otherwise not a token at all — a lone `&` stays the
    /// error it always was.
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    AmpEq,
    Lt,
    Le,
    Gt,
    Ge,
    Ne,
    Newline,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spanned {
    pub tok: Tok,
    pub line: usize,
    /// 1-based column of the token's first character. Together with `line` this
    /// is what lets the language server point at a symbol rather than a line.
    pub col: usize,
    /// One past the token's last byte, so a diagnostic can underline exactly
    /// the token — a string literal's spelling is longer than its value.
    pub end_col: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub line: usize,
    pub msg: String,
}

pub fn lex(src: &str) -> Result<Vec<Spanned>, LexError> {
    let mut out = Vec::new();
    let mut line = 1usize;
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let n = bytes.len();
    // Offset of the current line's first byte, so a column is `i - line_start`.
    let mut line_start = 0usize;

    // `end_col` is filled in after the arm has consumed the token: every arm
    // knows where its token starts, and only the loop knows where `i` ends up.
    let push = |out: &mut Vec<Spanned>, tok: Tok, line: usize, col: usize| {
        out.push(Spanned {
            tok,
            line,
            col,
            end_col: 0,
        })
    };

    while i < n {
        let c = bytes[i];
        // Column of the token about to be read, for language-server positions.
        let start_col = i - line_start + 1;
        let before = out.len();
        match c {
            b'\n' => {
                // Collapse runs of blank lines into a single Newline token so the
                // parser's statement loop stays simple.
                if !matches!(
                    out.last(),
                    Some(Spanned {
                        tok: Tok::Newline,
                        ..
                    }) | None
                ) {
                    push(&mut out, Tok::Newline, line, start_col);
                    // Measured here: the generic fix-up below would read the
                    // column against the line that starts after this token.
                    out.last_mut().unwrap().end_col = start_col + 1;
                }
                line += 1;
                i += 1;
                line_start = i;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'#' => {
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'(' => {
                push(&mut out, Tok::LParen, line, start_col);
                i += 1;
            }
            b')' => {
                push(&mut out, Tok::RParen, line, start_col);
                i += 1;
            }
            b'[' => {
                push(&mut out, Tok::LBracket, line, start_col);
                i += 1;
            }
            b']' => {
                push(&mut out, Tok::RBracket, line, start_col);
                i += 1;
            }
            b'{' => {
                push(&mut out, Tok::LBrace, line, start_col);
                i += 1;
            }
            b'}' => {
                push(&mut out, Tok::RBrace, line, start_col);
                i += 1;
            }
            b'?' => {
                push(&mut out, Tok::Question, line, start_col);
                i += 1;
            }
            b',' => {
                push(&mut out, Tok::Comma, line, start_col);
                i += 1;
            }
            b':' => {
                push(&mut out, Tok::Colon, line, start_col);
                i += 1;
            }
            // `..` (range) before a single `.` (field access): two dots fuse so
            // a range bound's expression ends at the `..` rather than reading
            // the first `.` as the start of a field access.
            b'.' if i + 1 < n && bytes[i + 1] == b'.' => {
                push(&mut out, Tok::DotDot, line, start_col);
                i += 2;
            }
            b'.' => {
                push(&mut out, Tok::Dot, line, start_col);
                i += 1;
            }
            b'=' => {
                push(&mut out, Tok::Eq, line, start_col);
                i += 1;
            }
            b'<' => {
                // `<=` and `<>` before bare `<`.
                if i + 1 < n && bytes[i + 1] == b'=' {
                    push(&mut out, Tok::Le, line, start_col);
                    i += 2;
                } else if i + 1 < n && bytes[i + 1] == b'>' {
                    push(&mut out, Tok::Ne, line, start_col);
                    i += 2;
                } else {
                    push(&mut out, Tok::Lt, line, start_col);
                    i += 1;
                }
            }
            b'>' => {
                if i + 1 < n && bytes[i + 1] == b'=' {
                    push(&mut out, Tok::Ge, line, start_col);
                    i += 2;
                } else {
                    push(&mut out, Tok::Gt, line, start_col);
                    i += 1;
                }
            }
            // `+=` etc. before the bare operator: a compound assignment is one
            // token so the parser reads it as an operator, not `+` then `=`.
            b'+' if i + 1 < n && bytes[i + 1] == b'=' => {
                push(&mut out, Tok::PlusEq, line, start_col);
                i += 2;
            }
            b'+' => {
                push(&mut out, Tok::Plus, line, start_col);
                i += 1;
            }
            b'-' if i + 1 < n && bytes[i + 1] == b'=' => {
                push(&mut out, Tok::MinusEq, line, start_col);
                i += 2;
            }
            b'-' => {
                push(&mut out, Tok::Minus, line, start_col);
                i += 1;
            }
            b'*' if i + 1 < n && bytes[i + 1] == b'=' => {
                push(&mut out, Tok::StarEq, line, start_col);
                i += 2;
            }
            b'*' => {
                push(&mut out, Tok::Star, line, start_col);
                i += 1;
            }
            b'/' if i + 1 < n && bytes[i + 1] == b'=' => {
                push(&mut out, Tok::SlashEq, line, start_col);
                i += 2;
            }
            b'/' => {
                push(&mut out, Tok::Slash, line, start_col);
                i += 1;
            }
            b'%' => {
                push(&mut out, Tok::Percent, line, start_col);
                i += 1;
            }
            // `&=` is text append. A lone `&` is not an operator in the language
            // (bitwise-and is the word `band`), so only the paired form is a
            // token; a stray `&` falls through to the unexpected-character error.
            b'&' if i + 1 < n && bytes[i + 1] == b'=' => {
                push(&mut out, Tok::AmpEq, line, start_col);
                i += 2;
            }
            // A block string, `"""..."""`, before the one-line `"..."`: three
            // quotes open a literal whose newlines are the newlines themselves,
            // so a paragraph, a SQL statement or a usage message is written the
            // way it will be printed. Its holes still fire — it is a text
            // literal, not a different kind of thing.
            b'"' if i + 2 < n && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' => {
                let open_line = line;
                let (s, ni) =
                    lex_quoted(bytes, i + 3, true, false, &mut line, &mut line_start)?;
                push(&mut out, Tok::Str(s), open_line, start_col);
                // A token that ran over lines has no honest end column on the
                // line it started on — every column after the newline is
                // measured from a later `line_start` — so the span covers the
                // opening delimiter and stops. One that stayed on its line
                // spans itself, as every other token does.
                out.last_mut().unwrap().end_col = if line == open_line {
                    ni - line_start + 1
                } else {
                    start_col + 3
                };
                i = ni;
            }
            b'"' => {
                let (s, ni) = lex_string(bytes, i + 1, line)?;
                push(&mut out, Tok::Str(s), line, start_col);
                i = ni;
            }
            // `r"..."` / `r"""..."""` — a raw literal, whose backslashes are
            // backslashes. It is what a Windows path and a regular expression
            // are made of, and writing either with doubled escapes is how a
            // wrong one gets shipped. `r` is a prefix, not a keyword: it means
            // this only when a quote follows it immediately, and two tokens in
            // a row (a name then a string) were never valid, so a variable
            // named `r` is untouched.
            b'r' if i + 1 < n && bytes[i + 1] == b'"' => {
                let triple = i + 3 < n && bytes[i + 2] == b'"' && bytes[i + 3] == b'"';
                let open_len = if triple { 4 } else { 2 };
                let open_line = line;
                let (s, ni) = lex_quoted(
                    bytes,
                    i + open_len,
                    triple,
                    true,
                    &mut line,
                    &mut line_start,
                )?;
                push(&mut out, Tok::RawStr(s), open_line, start_col);
                out.last_mut().unwrap().end_col = if line == open_line {
                    ni - line_start + 1
                } else {
                    start_col + open_len
                };
                i = ni;
            }
            // `0x...` / `0b...` — a bit pattern, before the decimal path, which
            // would otherwise read `0b1010` as the number 0 and the name
            // `b1010`.
            _ if c == b'0'
                && i + 1 < n
                && matches!(bytes[i + 1], b'x' | b'X' | b'b' | b'B') =>
            {
                let base = if matches!(bytes[i + 1], b'x' | b'X') { 16u32 } else { 2u32 };
                let start = i;
                i += 2;
                let digits_start = i;
                let mut value: u64 = 0;
                let mut digits = 0usize;
                while i < n && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric()) {
                    if bytes[i] == b'_' {
                        i += 1;
                        continue;
                    }
                    let d = (bytes[i] as char).to_digit(base).ok_or_else(|| LexError {
                        line,
                        msg: format!(
                            "`{}` is not a {} digit in `{}`",
                            bytes[i] as char,
                            if base == 16 { "hexadecimal" } else { "binary" },
                            &src[start..i + 1]
                        ),
                    })?;
                    // More than 64 bits has no type to land in, and silently
                    // dropping the top of a bit pattern is the worst possible
                    // answer.
                    value = value
                        .checked_mul(base as u64)
                        .and_then(|v| v.checked_add(d as u64))
                        .ok_or_else(|| LexError {
                            line,
                            msg: format!(
                                "the literal `{}` needs more than 64 bits",
                                &src[start..i + 1]
                            ),
                        })?;
                    digits += 1;
                    i += 1;
                }
                if digits == 0 {
                    return Err(LexError {
                        line,
                        msg: format!(
                            "`{}` has no digits after it",
                            &src[start..digits_start]
                        ),
                    });
                }
                push(&mut out, Tok::Bits(value), line, start_col);
            }
            _ if c.is_ascii_digit() => {
                let start = i;
                // `1_000_000` — a digit-grouping underscore, matching the hex /
                // binary path above. An underscore is allowed only between two
                // digits, so `1_`, `_1` (which lexes as an identifier anyway)
                // and `1__0` are refused rather than silently accepted.
                scan_dec_digits(bytes, &mut i, n, line)?;
                // Fractional part -> floating-point literal (needs a digit after `.`).
                let is_float = i + 1 < n && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit();
                if is_float {
                    i += 1; // consume '.'
                    scan_dec_digits(bytes, &mut i, n, line)?;
                }
                // Parse the value without the grouping underscores; the span
                // still covers the digits as written, for the diagnostic.
                let raw = &src[start..i];
                let text: String = raw.bytes().filter(|&b| b != b'_').map(|b| b as char).collect();
                if is_float {
                    let v: f64 = text.parse().map_err(|_| LexError {
                        line,
                        msg: format!("invalid float literal: {raw}"),
                    })?;
                    push(&mut out, Tok::Float(v), line, start_col);
                } else {
                    let v: i64 = text.parse().map_err(|_| LexError {
                        line,
                        msg: format!("integer literal out of range: {raw}"),
                    })?;
                    push(&mut out, Tok::Int(v), line, start_col);
                }
            }
            _ if is_ident_start(c) => {
                let start = i;
                while i < n && is_ident_cont(bytes[i]) {
                    i += 1;
                }
                let word = &src[start..i];
                let tok = match word {
                    "module" => Tok::Module,
                    "sub" => Tok::Sub,
                    "end" => Tok::End,
                    "let" => Tok::Let,
                    "var" => Tok::Var,
                    "if" => Tok::If,
                    "else" => Tok::Else,
                    "while" => Tok::While,
                    "for" => Tok::For,
                    "break" => Tok::Break,
                    "continue" => Tok::Continue,
                    "and" => Tok::And,
                    "or" => Tok::Or,
                    "not" => Tok::Not,
                    "bnot" => Tok::BNot,
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "call" => Tok::Call,
                    "use" => Tok::Use,
                    "form" => Tok::Form,
                    "on" => Tok::On,
                    "return" => Tok::Return,
                    _ => Tok::Ident(word.to_string()),
                };
                push(&mut out, tok, line, start_col);
            }
            _ => {
                return Err(LexError {
                    line,
                    msg: format!("unexpected character: {:?}", c as char),
                });
            }
        }
        if out.len() > before {
            let last = out.last_mut().unwrap();
            if last.end_col == 0 {
                last.end_col = i - line_start + 1;
            }
        }
    }
    out.push(Spanned {
        tok: Tok::Eof,
        line,
        col: i - line_start + 1,
        end_col: i - line_start + 1,
    });
    Ok(out)
}

/// Consume a run of decimal digits with optional grouping underscores, starting
/// at a digit. An underscore must sit between two digits — `bytes[*i]` is a
/// digit on entry, so the "digit before" half is given, and the following byte
/// is required to be a digit too. `1_000` advances past all five characters;
/// `1_` and `1__0` are refused with a line number.
fn scan_dec_digits(bytes: &[u8], i: &mut usize, n: usize, line: usize) -> Result<(), LexError> {
    while *i < n {
        if bytes[*i].is_ascii_digit() {
            *i += 1;
        } else if bytes[*i] == b'_' {
            if *i + 1 < n && bytes[*i + 1].is_ascii_digit() {
                *i += 1;
            } else {
                return Err(LexError {
                    line,
                    msg: "an underscore in a number must sit between two digits".into(),
                });
            }
        } else {
            break;
        }
    }
    Ok(())
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}
fn is_ident_cont(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

/// Scan a block (`"""`) or raw (`r"`) string body, starting just after the
/// opening delimiter; returns the decoded text and the index just past the
/// closing one.
///
/// `line` and `line_start` are advanced past every newline the body swallows,
/// because a block literal is the one token that can end on a different line
/// than it began on and every column after it is measured from the last one.
///
/// `triple` says the delimiter is three quotes — which is also what allows a
/// newline inside — and `raw` says the body is its own bytes: no escapes, and
/// (in the parser) no interpolation.
fn lex_quoted(
    bytes: &[u8],
    mut i: usize,
    triple: bool,
    raw: bool,
    line: &mut usize,
    line_start: &mut usize,
) -> Result<(String, usize), LexError> {
    let n = bytes.len();
    let open_line = *line;
    let mut s = String::new();
    // One newline directly after the opening `"""` is dropped, so a block can
    // start on the line after its delimiter — which is how one is written — and
    // still not begin with a blank line. Exactly one, and nothing else: the
    // body's indentation is its own, so what is between the delimiters is what
    // comes out, with no second rule to remember.
    if triple && i < n {
        if bytes[i] == b'\n' {
            i += 1;
            *line += 1;
            *line_start = i;
        } else if bytes[i] == b'\r' && i + 1 < n && bytes[i + 1] == b'\n' {
            i += 2;
            *line += 1;
            *line_start = i;
        }
    }
    loop {
        if i >= n {
            return Err(LexError {
                line: open_line,
                msg: if triple {
                    "unterminated block string literal — it needs a closing `\"\"\"`".into()
                } else {
                    "unterminated string literal".into()
                },
            });
        }
        match bytes[i] {
            // In a block, a lone `"` is content and only three in a row close.
            b'"' if triple => {
                if i + 2 < n && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
                    return Ok((s, i + 3));
                }
                s.push('"');
                i += 1;
            }
            b'"' => return Ok((s, i + 1)),
            // CRLF becomes LF, so a literal means the same thing whichever way
            // the file was saved.
            b'\r' if triple && i + 1 < n && bytes[i + 1] == b'\n' => i += 1,
            b'\n' if triple => {
                s.push('\n');
                i += 1;
                *line += 1;
                *line_start = i;
            }
            b'\n' => {
                return Err(LexError {
                    line: *line,
                    msg: "newline in string literal — `\"\"\"` opens one that may run over lines"
                        .into(),
                })
            }
            b'\\' if !raw => {
                i += 1;
                if i >= n {
                    return Err(LexError {
                        line: *line,
                        msg: "unterminated escape".into(),
                    });
                }
                match bytes[i] {
                    b'n' => s.push('\n'),
                    b't' => s.push('\t'),
                    b'\\' => s.push('\\'),
                    b'"' => s.push('"'),
                    b'0' => s.push('\0'),
                    other => {
                        return Err(LexError {
                            line: *line,
                            msg: format!("unknown escape: \\{}", other as char),
                        })
                    }
                }
                i += 1;
            }
            _ => {
                // A run of ordinary bytes. In a raw literal a backslash is one
                // of them, which is the whole point of the form.
                let start = i;
                while i < n
                    && bytes[i] != b'"'
                    && bytes[i] != b'\n'
                    && bytes[i] != b'\r'
                    && !(bytes[i] == b'\\' && !raw)
                {
                    i += 1;
                }
                if i == start {
                    // A lone `\r` outside a block, or a `\r` not before a `\n`:
                    // carried through as the byte it is.
                    s.push(bytes[i] as char);
                    i += 1;
                } else {
                    s.push_str(std::str::from_utf8(&bytes[start..i]).map_err(|_| LexError {
                        line: open_line,
                        msg: "invalid UTF-8 in string literal".into(),
                    })?);
                }
            }
        }
    }
}

/// Lex a string body starting just after the opening quote; returns the decoded
/// string and the index just past the closing quote.  Supports `\n \t \\ \" \0`.
fn lex_string(bytes: &[u8], mut i: usize, line: usize) -> Result<(String, usize), LexError> {
    let mut s = String::new();
    let n = bytes.len();
    loop {
        if i >= n {
            return Err(LexError {
                line,
                msg: "unterminated string literal".into(),
            });
        }
        match bytes[i] {
            b'"' => return Ok((s, i + 1)),
            b'\n' => {
                return Err(LexError {
                    line,
                    msg: "newline in string literal".into(),
                })
            }
            b'\\' => {
                i += 1;
                if i >= n {
                    return Err(LexError {
                        line,
                        msg: "unterminated escape".into(),
                    });
                }
                match bytes[i] {
                    b'n' => s.push('\n'),
                    b't' => s.push('\t'),
                    b'\\' => s.push('\\'),
                    b'"' => s.push('"'),
                    b'0' => s.push('\0'),
                    other => {
                        return Err(LexError {
                            line,
                            msg: format!("unknown escape: \\{}", other as char),
                        })
                    }
                }
                i += 1;
            }
            _ => {
                // Pass raw bytes through as-is (UTF-8 preserved).
                let start = i;
                while i < n && bytes[i] != b'"' && bytes[i] != b'\\' && bytes[i] != b'\n' {
                    i += 1;
                }
                s.push_str(std::str::from_utf8(&bytes[start..i]).map_err(|_| LexError {
                    line,
                    msg: "invalid UTF-8 in string literal".into(),
                })?);
            }
        }
    }
}
