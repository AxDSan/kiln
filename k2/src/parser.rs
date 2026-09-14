//! The K2 parser: tokens → AST. Recursive descent with a precedence climb for
//! expressions. Covers the subset the first running compiler handles; it is
//! written to grow (generics/lambdas/attributes/forms) without restructuring.

use crate::ast::*;
use crate::lexer::{InterpPart, Kw, Span, Spanned, Tok};

pub struct ParseError {
    pub msg: String,
    pub span: Span,
}

pub fn parse(toks: Vec<Spanned>) -> Result<Program, ParseError> {
    let mut p = Parser { toks, i: 0 };
    p.program()
}

struct Parser {
    toks: Vec<Spanned>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.i].tok
    }
    fn peek_at(&self, n: usize) -> &Tok {
        self.toks
            .get(self.i + n)
            .map(|s| &s.tok)
            .unwrap_or(&Tok::Eof)
    }
    fn span(&self) -> Span {
        self.toks[self.i].span
    }
    fn doc(&self) -> Option<String> {
        self.toks[self.i].doc.clone()
    }
    /// Comments written above whatever starts at the current token.
    fn leading(&self) -> Vec<String> {
        self.toks[self.i].leading.clone()
    }
    fn bump(&mut self) -> Tok {
        let t = self.toks[self.i].tok.clone();
        if self.i + 1 < self.toks.len() {
            self.i += 1;
        }
        t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == t {
            self.bump();
            true
        } else {
            false
        }
    }
    fn eat_kw(&mut self, kw: Kw) -> bool {
        if self.peek() == &Tok::Keyword(kw) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            msg: msg.into(),
            span: self.span(),
        })
    }
    fn expect(&mut self, t: &Tok) -> Result<(), ParseError> {
        if self.eat(t) {
            Ok(())
        } else {
            self.err(format!("expected {t:?}, found {:?}", self.peek()))
        }
    }
    fn ident(&mut self) -> Result<String, ParseError> {
        match self.bump() {
            Tok::Ident(s) => Ok(s),
            other => {
                self.i -= 1;
                self.err(format!("expected identifier, found {other:?}"))
            }
        }
    }

    // ─── program ──────────────────────────────────────────────────────────

    fn program(&mut self) -> Result<Program, ParseError> {
        let file_leading = self.leading();
        let mut namespace = None;
        if self.peek() == &Tok::Keyword(Kw::Namespace) {
            self.bump();
            let path = self.dotted_name()?;
            // file-scoped `namespace X;` or block `namespace X { }`
            if self.eat(&Tok::Semi) {
                namespace = Some(path);
            } else {
                self.expect(&Tok::LBrace)?;
                namespace = Some(path);
                // block body handled below; closing brace consumed at end
            }
        }
        let mut usings = Vec::new();
        while self.peek() == &Tok::Keyword(Kw::Using) {
            usings.push(self.using()?);
        }
        let mut items = Vec::new();
        let mut top_level = Vec::new();
        loop {
            match self.peek() {
                Tok::Eof => break,
                Tok::RBrace => {
                    self.bump();
                    break;
                }
                Tok::LBracket => items.push(self.item()?),
                _ if self.starts_type_decl() => items.push(self.item()?),
                _ => top_level.push(self.stmt()?),
            }
        }
        Ok(Program {
            leading: file_leading,
            namespace,
            usings,
            items,
            top_level,
        })
    }

    fn dotted_name(&mut self) -> Result<String, ParseError> {
        let mut s = self.ident()?;
        while self.peek() == &Tok::Dot {
            self.bump();
            s.push('.');
            s.push_str(&self.ident()?);
        }
        Ok(s)
    }

    fn using(&mut self) -> Result<Using, ParseError> {
        let span = self.span();
        self.expect(&Tok::Keyword(Kw::Using))?;
        let is_static = self.eat_kw(Kw::Static);
        // `using Alias = X.Y;` vs `using X.Y;`
        if matches!(self.peek(), Tok::Ident(_)) && self.peek_at(1) == &Tok::Eq {
            let alias = self.ident()?;
            self.expect(&Tok::Eq)?;
            let path = self.dotted_name()?;
            self.expect(&Tok::Semi)?;
            return Ok(Using {
                path,
                is_static,
                alias: Some(alias),
                span,
            });
        }
        let path = self.dotted_name()?;
        self.expect(&Tok::Semi)?;
        Ok(Using {
            path,
            is_static,
            alias: None,
            span,
        })
    }

    fn starts_type_decl(&self) -> bool {
        let mut n = 0;
        // skip leading modifiers
        loop {
            match self.peek_at(n) {
                Tok::Keyword(Kw::Public | Kw::Internal | Kw::Private | Kw::Static) => n += 1,
                Tok::Ident(w) if w == "partial" => n += 1,
                _ => break,
            }
        }
        matches!(
            self.peek_at(n),
            Tok::Keyword(Kw::Class | Kw::Record | Kw::Struct | Kw::Enum | Kw::Interface | Kw::Form)
        )
    }

    // ─── items ────────────────────────────────────────────────────────────

    fn modifiers(&mut self) -> (Vis, bool) {
        let mut vis = Vis::Internal;
        let mut is_static = false;
        loop {
            match self.peek() {
                Tok::Keyword(Kw::Public) => {
                    vis = Vis::Public;
                    self.bump();
                }
                Tok::Keyword(Kw::Internal) => {
                    vis = Vis::Internal;
                    self.bump();
                }
                Tok::Keyword(Kw::Private) => {
                    vis = Vis::Private;
                    self.bump();
                }
                Tok::Keyword(Kw::Static) => {
                    is_static = true;
                    self.bump();
                }
                // `partial` splits a declaration across blocks; the parts are
                // merged when lowering.
                Tok::Ident(w) if w == "partial" => {
                    self.bump();
                }
                _ => break,
            }
        }
        (vis, is_static)
    }

    fn item(&mut self) -> Result<Item, ParseError> {
        let doc = self.doc();
        let leading = self.leading();
        let attrs = self.attributes()?;
        let span = self.span();
        let (vis, is_static) = self.modifiers();
        match self.bump() {
            Tok::Keyword(Kw::Enum) => Ok(Item::Enum(self.enum_decl(vis, doc, leading, span)?)),
            Tok::Keyword(Kw::Form) => {
                let name = self.ident()?;
                self.expect(&Tok::LBrace)?;
                let mut properties = Vec::new();
                let mut components = Vec::new();
                let mut fields = Vec::new();
                let mut consts = Vec::new();
                let mut methods = Vec::new();
                while self.peek() != &Tok::RBrace {
                    // `Prop = value;`
                    if matches!(self.peek(), Tok::Ident(_)) && self.peek_at(1) == &Tok::Eq {
                        let pname = self.ident()?;
                        self.expect(&Tok::Eq)?;
                        let v = self.expr()?;
                        self.expect(&Tok::Semi)?;
                        properties.push((pname, v));
                        continue;
                    }
                    // `Type id { … }` — a component
                    if matches!(self.peek(), Tok::Ident(_))
                        && matches!(self.peek_at(1), Tok::Ident(_))
                        && self.peek_at(2) == &Tok::LBrace
                    {
                        let cspan = self.span();
                        let cleading = self.leading();
                        let type_name = self.ident()?;
                        let id = self.ident()?;
                        self.expect(&Tok::LBrace)?;
                        let mut cprops = Vec::new();
                        let mut handlers = Vec::new();
                        while self.peek() != &Tok::RBrace {
                            let n = self.ident()?;
                            if self.eat(&Tok::PlusEq) {
                                // a method by name, or a lambda written here
                                if let Some(l) = self.try_lambda()? {
                                    match l.kind {
                                        ExprKind::Lambda(lam) => {
                                            handlers.push((n, HandlerRef::Lambda(lam)))
                                        }
                                        _ => unreachable!(),
                                    }
                                } else {
                                    handlers.push((n, HandlerRef::Method(self.ident()?)));
                                }
                            } else {
                                self.expect(&Tok::Eq)?;
                                cprops.push((n, self.expr()?));
                            }
                            self.expect(&Tok::Semi)?;
                        }
                        self.expect(&Tok::RBrace)?;
                        components.push(ComponentDecl {
                            leading: cleading,
                            type_name,
                            id,
                            properties: cprops,
                            handlers,
                            span: cspan,
                        });
                        continue;
                    }
                    self.member(&name, &mut fields, &mut consts, &mut methods)?;
                }
                self.expect(&Tok::RBrace)?;
                Ok(Item::Form(FormDecl {
                    leading,
                    vis,
                    name,
                    properties,
                    components,
                    fields,
                    methods,
                    doc,
                    span,
                }))
            }
            Tok::Keyword(Kw::Interface) => {
                let name = self.ident()?;
                self.expect(&Tok::LBrace)?;
                let mut methods = Vec::new();
                while self.peek() != &Tok::RBrace {
                    let mspan = self.span();
                    let (mvis, _) = self.modifiers();
                    let ret = self.type_ref()?;
                    let mname = self.ident()?;
                    let params = self.params()?;
                    self.expect(&Tok::Semi)?;
                    methods.push(Method {
                        leading: Vec::new(),
                        attrs: Vec::new(),
                        is_extern: false,
                        vis: mvis,
                        is_static: false,
                        name: mname,
                        type_params: Vec::new(),
                        constraints: Vec::new(),
                        params,
                        ret,
                        body: Vec::new(),
                        expr_body: None,
                        doc: None,
                        span: mspan,
                    });
                }
                self.expect(&Tok::RBrace)?;
                Ok(Item::Interface(InterfaceDecl {
                    leading,
                    vis,
                    name,
                    methods,
                    doc,
                    span,
                }))
            }
            Tok::Keyword(kw @ (Kw::Class | Kw::Record | Kw::Struct)) => {
                let kind = match (kw, is_static) {
                    (Kw::Class, true) => TypeKind::StaticClass,
                    (Kw::Class, false) => TypeKind::Class,
                    (Kw::Record, _) => TypeKind::Record,
                    (Kw::Struct, _) => TypeKind::Struct,
                    _ => unreachable!(),
                };
                Ok(Item::Type(
                    self.type_decl(kind, vis, doc, leading, span, attrs)?,
                ))
            }
            other => {
                self.i -= 1;
                self.err(format!("expected a type declaration, found {other:?}"))
            }
        }
    }

    fn enum_decl(
        &mut self,
        vis: Vis,
        doc: Option<String>,
        leading: Vec<String>,
        span: Span,
    ) -> Result<EnumDecl, ParseError> {
        let name = self.ident()?;
        let backing = if self.eat(&Tok::Colon) {
            Some(self.type_ref()?)
        } else {
            None
        };
        self.expect(&Tok::LBrace)?;
        let mut members = Vec::new();
        while self.peek() != &Tok::RBrace {
            let m = self.ident()?;
            let v = if self.eat(&Tok::Eq) {
                Some(self.expr()?)
            } else {
                None
            };
            members.push((m, v));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RBrace)?;
        Ok(EnumDecl {
            leading,
            vis,
            name,
            backing,
            members,
            doc,
            span,
        })
    }

    fn type_decl(
        &mut self,
        kind: TypeKind,
        vis: Vis,
        doc: Option<String>,
        leading: Vec<String>,
        span: Span,
        attrs: Vec<Attribute>,
    ) -> Result<TypeDecl, ParseError> {
        let name = self.ident()?;
        let type_params = self.opt_type_params()?;
        // positional record params
        let mut record_params = Vec::new();
        if self.peek() == &Tok::LParen {
            self.bump();
            while self.peek() != &Tok::RParen {
                let fattrs = self.attributes()?;
                let pspan = self.span();
                let ty = self.type_ref()?;
                let pname = self.ident()?;
                record_params.push(Field {
                    leading: Vec::new(),
                    attrs: fattrs,
                    vis: Vis::Public,
                    name: pname,
                    ty,
                    default: None,
                    is_readonly: true,
                    is_const: false,
                    span: pspan,
                });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
        }
        // `: I, J` — interfaces this type implements. There is no inheritance.
        let mut implements = Vec::new();
        if self.eat(&Tok::Colon) {
            loop {
                if let TypeRef::Named(n) = self.type_ref()? {
                    implements.push(n);
                }
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        let mut fields = Vec::new();
        let mut consts = Vec::new();
        let mut methods = Vec::new();
        // A record with only positional params may end in `;`.
        if self.eat(&Tok::Semi) {
            return Ok(TypeDecl {
                leading: leading.clone(),
                attrs: attrs.clone(),
                implements: implements.clone(),
                type_params: type_params.clone(),
                kind,
                vis,
                name,
                record_params,
                fields,
                consts,
                methods,
                doc,
                span,
            });
        }
        self.expect(&Tok::LBrace)?;
        while self.peek() != &Tok::RBrace {
            self.member(&name, &mut fields, &mut consts, &mut methods)?;
        }
        self.expect(&Tok::RBrace)?;
        Ok(TypeDecl {
            leading,
            attrs,
            implements,
            type_params,
            kind,
            vis,
            name,
            record_params,
            fields,
            consts,
            methods,
            doc,
            span,
        })
    }

    /// `[Name(args)]` sequences before a declaration.
    fn attributes(&mut self) -> Result<Vec<Attribute>, ParseError> {
        let mut out = Vec::new();
        while self.peek() == &Tok::LBracket {
            self.bump();
            loop {
                let name = self.ident()?;
                let mut args = Vec::new();
                let mut named = Vec::new();
                if self.peek() == &Tok::LParen {
                    self.bump();
                    while self.peek() != &Tok::RParen {
                        if matches!(self.peek(), Tok::Ident(_)) && self.peek_at(1) == &Tok::Eq {
                            let k = self.ident()?;
                            self.expect(&Tok::Eq)?;
                            named.push((k, self.expr()?));
                        } else {
                            args.push(self.expr()?);
                        }
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RParen)?;
                }
                out.push(Attribute { name, args, named });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RBracket)?;
        }
        Ok(out)
    }

    fn member(
        &mut self,
        type_name: &str,
        fields: &mut Vec<Field>,
        consts: &mut Vec<ConstDecl>,
        methods: &mut Vec<Method>,
    ) -> Result<(), ParseError> {
        let doc = self.doc();
        let leading = self.leading();
        let attrs = self.attributes()?;
        let span = self.span();
        let (vis, is_static) = self.modifiers();
        let is_extern = if self.peek() == &Tok::Ident("extern".into()) {
            self.bump();
            true
        } else {
            false
        };
        let (vis, is_static) = if is_extern {
            let (v2, s2) = self.modifiers();
            (if v2 == Vis::Internal { vis } else { v2 }, is_static || s2)
        } else {
            (vis, is_static)
        };
        let is_const = self.eat_kw(Kw::Const);
        let is_readonly = if !is_const {
            // `readonly` is a contextual ident in our lexer
            if self.peek() == &Tok::Ident("readonly".into()) {
                self.bump();
                true
            } else {
                false
            }
        } else {
            false
        };

        // A constructor: the type's own name, then a parameter list.
        if self.peek() == &Tok::Ident(type_name.to_string()) && self.peek_at(1) == &Tok::LParen {
            self.bump();
            let params = self.params()?;
            let body = self.block()?;
            methods.push(Method {
                leading: leading.clone(),
                attrs,
                is_extern: false,
                vis,
                is_static: true,
                name: "$ctor".into(),
                type_params: Vec::new(),
                constraints: Vec::new(),
                params,
                ret: TypeRef::Named(type_name.to_string()),
                body,
                expr_body: None,
                doc,
                span,
            });
            return Ok(());
        }

        let ty = self.type_ref()?;
        let name = self.ident()?;

        // method? (optionally generic: `Name<T, U>(...)`)
        let type_params = self.opt_type_params()?;
        if self.peek() == &Tok::LParen {
            let params = self.params()?;
            let constraints = self.opt_where_clause()?;
            let (body, expr_body) = if is_extern {
                self.expect(&Tok::Semi)?;
                (Vec::new(), None)
            } else {
                self.method_body()?
            };
            methods.push(Method {
                leading: leading.clone(),
                attrs,
                is_extern,
                vis,
                is_static,
                name,
                type_params,
                constraints,
                params,
                ret: ty,
                body,
                expr_body,
                doc,
                span,
            });
            return Ok(());
        }

        // const / field
        if is_const {
            self.expect(&Tok::Eq)?;
            let value = self.expr()?;
            self.expect(&Tok::Semi)?;
            consts.push(ConstDecl {
                leading,
                vis,
                name,
                ty,
                value,
                span,
            });
            return Ok(());
        }

        let default = if self.eat(&Tok::Eq) {
            Some(self.expr()?)
        } else {
            None
        };
        self.expect(&Tok::Semi)?;
        fields.push(Field {
            leading,
            attrs,
            vis,
            name,
            ty,
            default,
            is_readonly,
            is_const: false,
            span,
        });
        Ok(())
    }

    /// `<T, U>` after a method name, or nothing.
    fn opt_type_params(&mut self) -> Result<Vec<String>, ParseError> {
        if self.peek() != &Tok::Lt {
            return Ok(Vec::new());
        }
        self.bump();
        let mut out = Vec::new();
        loop {
            out.push(self.ident()?);
            if self.eat(&Tok::Comma) {
                continue;
            }
            break;
        }
        self.close_generic()?;
        Ok(out)
    }

    /// `where T : I1, U : I2` — the constraints a type argument must satisfy.
    fn opt_where_clause(&mut self) -> Result<Vec<(String, String)>, ParseError> {
        let mut out = Vec::new();
        if self.peek() != &Tok::Ident("where".into()) {
            return Ok(out);
        }
        self.bump();
        loop {
            let tp = self.ident()?;
            self.expect(&Tok::Colon)?;
            loop {
                if let TypeRef::Named(n) = self.type_ref()? {
                    out.push((tp.clone(), n));
                }
                if !self.eat(&Tok::Comma) {
                    break;
                }
                if matches!(self.peek(), Tok::Ident(_)) && self.peek_at(1) == &Tok::Colon {
                    break;
                }
            }
            if self.peek() == &Tok::LBrace || self.peek() == &Tok::FatArrow {
                break;
            }
            if self.peek() == &Tok::Ident("where".into()) {
                self.bump();
                continue;
            }
            if matches!(self.peek(), Tok::Ident(_)) && self.peek_at(1) == &Tok::Colon {
                continue;
            }
            break;
        }
        Ok(out)
    }

    fn params(&mut self) -> Result<Vec<Param>, ParseError> {
        self.expect(&Tok::LParen)?;
        let mut out = Vec::new();
        while self.peek() != &Tok::RParen {
            let span = self.span();
            // ignore ref/out markers for now
            let _ = self.eat_kw(Kw::Ref) || self.eat_kw(Kw::Out);
            let ty = self.type_ref()?;
            let name = self.ident()?;
            // default values parsed and ignored for now
            if self.eat(&Tok::Eq) {
                self.expr()?;
            }
            out.push(Param { name, ty, span });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(out)
    }

    fn method_body(&mut self) -> Result<(Vec<Stmt>, Option<Expr>), ParseError> {
        if self.eat(&Tok::FatArrow) {
            let e = self.expr()?;
            self.expect(&Tok::Semi)?;
            return Ok((Vec::new(), Some(e)));
        }
        let body = self.block()?;
        Ok((body, None))
    }

    // ─── types ──────────────────────────────────────────────────────────────

    fn type_ref(&mut self) -> Result<TypeRef, ParseError> {
        if self.eat_kw(Kw::Void) {
            return Ok(TypeRef::Void);
        }
        let name = self.ident()?;
        let mut ty = if self.peek() == &Tok::Lt {
            self.bump();
            let mut args = Vec::new();
            loop {
                args.push(self.type_ref()?);
                if self.eat(&Tok::Comma) {
                    continue;
                }
                break;
            }
            self.close_generic()?;
            TypeRef::Generic(name, args)
        } else {
            TypeRef::Named(name)
        };
        loop {
            match self.peek() {
                Tok::Question => {
                    self.bump();
                    ty = TypeRef::Optional(Box::new(ty));
                }
                Tok::LBracket if self.peek_at(1) == &Tok::RBracket => {
                    self.bump();
                    self.bump();
                    ty = TypeRef::Array(Box::new(ty));
                }
                _ => break,
            }
        }
        Ok(ty)
    }

    /// Close a generic argument list, splitting a `>>` token into two `>`.
    fn close_generic(&mut self) -> Result<(), ParseError> {
        match self.peek() {
            Tok::Gt => {
                self.bump();
                Ok(())
            }
            Tok::Shr => {
                // consume one `>`, leave a `>` in place
                self.toks[self.i].tok = Tok::Gt;
                Ok(())
            }
            Tok::Ge => {
                self.toks[self.i].tok = Tok::Eq;
                Ok(())
            }
            _ => self.err("expected `>` to close generic arguments"),
        }
    }

    // ─── statements ─────────────────────────────────────────────────────────

    fn block(&mut self) -> Result<Vec<Stmt>, ParseError> {
        self.expect(&Tok::LBrace)?;
        let mut out = Vec::new();
        while self.peek() != &Tok::RBrace && self.peek() != &Tok::Eof {
            out.push(self.stmt()?);
        }
        self.expect(&Tok::RBrace)?;
        Ok(out)
    }

    fn stmt(&mut self) -> Result<Stmt, ParseError> {
        let leading = self.leading();
        let mut s = self.stmt_inner()?;
        s.leading = leading;
        Ok(s)
    }

    fn stmt_inner(&mut self) -> Result<Stmt, ParseError> {
        let span = self.span();
        let kind = match self.peek() {
            Tok::LBrace => StmtKind::Block(self.block()?),
            Tok::Keyword(Kw::Return) => {
                self.bump();
                let v = if self.peek() == &Tok::Semi {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect(&Tok::Semi)?;
                StmtKind::Return(v)
            }
            Tok::Keyword(Kw::If) => return self.if_stmt(),
            Tok::Keyword(Kw::While) => {
                self.bump();
                self.expect(&Tok::LParen)?;
                let cond = self.expr()?;
                self.expect(&Tok::RParen)?;
                let body = self.body_or_stmt()?;
                StmtKind::While { cond, body }
            }
            Tok::Keyword(Kw::For) => return self.for_stmt(),
            Tok::Keyword(Kw::Foreach) => return self.foreach_stmt(),
            Tok::Keyword(Kw::Break) => {
                self.bump();
                self.expect(&Tok::Semi)?;
                StmtKind::Break
            }
            Tok::Keyword(Kw::Continue) => {
                self.bump();
                self.expect(&Tok::Semi)?;
                StmtKind::Continue
            }
            Tok::Keyword(Kw::Defer) => {
                self.bump();
                let s = self.stmt()?;
                StmtKind::Defer(Box::new(s))
            }
            Tok::Keyword(Kw::Var) | Tok::Keyword(Kw::Let) => {
                let mutable = self.peek() == &Tok::Keyword(Kw::Var);
                self.bump();
                let name = self.ident()?;
                self.expect(&Tok::Eq)?;
                let value = self.expr()?;
                self.expect(&Tok::Semi)?;
                StmtKind::Local {
                    name,
                    ty: None,
                    mutable,
                    value,
                }
            }
            _ => return self.expr_or_decl_stmt(),
        };
        Ok(Stmt {
            leading: Vec::new(),
            kind,
            span,
        })
    }

    fn body_or_stmt(&mut self) -> Result<Vec<Stmt>, ParseError> {
        if self.peek() == &Tok::LBrace {
            self.block()
        } else {
            Ok(vec![self.stmt()?])
        }
    }

    fn if_stmt(&mut self) -> Result<Stmt, ParseError> {
        let span = self.span();
        self.expect(&Tok::Keyword(Kw::If))?;
        self.expect(&Tok::LParen)?;
        let cond = self.expr()?;
        self.expect(&Tok::RParen)?;
        let then = self.body_or_stmt()?;
        let els = if self.eat_kw(Kw::Else) {
            if self.peek() == &Tok::Keyword(Kw::If) {
                vec![self.if_stmt()?]
            } else {
                self.body_or_stmt()?
            }
        } else {
            Vec::new()
        };
        Ok(Stmt {
            leading: Vec::new(),
            kind: StmtKind::If { cond, then, els },
            span,
        })
    }

    fn for_stmt(&mut self) -> Result<Stmt, ParseError> {
        let span = self.span();
        self.expect(&Tok::Keyword(Kw::For))?;
        self.expect(&Tok::LParen)?;
        let init = if self.peek() == &Tok::Semi {
            self.bump();
            None
        } else {
            let s = self.simple_stmt()?;
            self.expect(&Tok::Semi)?;
            Some(s)
        };
        let cond = if self.peek() == &Tok::Semi {
            None
        } else {
            Some(self.expr()?)
        };
        self.expect(&Tok::Semi)?;
        let step = if self.peek() == &Tok::RParen {
            None
        } else {
            Some(self.simple_stmt()?)
        };
        self.expect(&Tok::RParen)?;
        let body = self.body_or_stmt()?;
        Ok(Stmt {
            leading: Vec::new(),
            kind: StmtKind::For {
                init: Box::new(init),
                cond,
                step: Box::new(step),
                body,
            },
            span,
        })
    }

    fn foreach_stmt(&mut self) -> Result<Stmt, ParseError> {
        let span = self.span();
        self.expect(&Tok::Keyword(Kw::Foreach))?;
        self.expect(&Tok::LParen)?;
        // `var x`, `T x`, or `var (key, value)` over a dictionary.
        let is_var = self.eat_kw(Kw::Var);
        let (var, value) = if is_var && self.peek() == &Tok::LParen {
            self.bump();
            let k = self.ident()?;
            self.expect(&Tok::Comma)?;
            let v = self.ident()?;
            self.expect(&Tok::RParen)?;
            (k, Some(v))
        } else {
            if !is_var {
                self.type_ref()?; // explicit element type, ignored (inferred)
            }
            (self.ident()?, None)
        };
        self.expect(&Tok::Keyword(Kw::In))?;
        let coll = self.expr()?;
        self.expect(&Tok::RParen)?;
        let body = self.body_or_stmt()?;
        Ok(Stmt {
            leading: Vec::new(),
            kind: StmtKind::ForEach {
                var,
                value,
                coll,
                body,
            },
            span,
        })
    }

    /// A statement without a trailing `;` requirement (for `for` clauses).
    fn simple_stmt(&mut self) -> Result<Stmt, ParseError> {
        let span = self.span();
        if self.peek() == &Tok::Keyword(Kw::Var) || self.peek() == &Tok::Keyword(Kw::Let) {
            let mutable = self.peek() == &Tok::Keyword(Kw::Var);
            self.bump();
            let name = self.ident()?;
            self.expect(&Tok::Eq)?;
            let value = self.expr()?;
            return Ok(Stmt {
                leading: Vec::new(),
                kind: StmtKind::Local {
                    name,
                    ty: None,
                    mutable,
                    value,
                },
                span,
            });
        }
        self.assign_or_expr(span)
    }

    /// Either a typed local declaration (`T name = e;`) or an expression /
    /// assignment statement. Uses a speculative parse to tell them apart.
    fn expr_or_decl_stmt(&mut self) -> Result<Stmt, ParseError> {
        let span = self.span();
        if let Some(decl) = self.try_local_decl(span)? {
            return Ok(decl);
        }
        let s = self.assign_or_expr(span)?;
        self.expect(&Tok::Semi)?;
        Ok(s)
    }

    fn try_local_decl(&mut self, span: Span) -> Result<Option<Stmt>, ParseError> {
        let save = self.i;
        // A declaration begins with a type then an identifier.
        let Ok(ty) = self.type_ref() else {
            self.i = save;
            return Ok(None);
        };
        if let Tok::Ident(name) = self.peek().clone() {
            let after = self.peek_at(1).clone();
            if after == Tok::Eq || after == Tok::Semi {
                self.bump(); // name
                let value = if self.eat(&Tok::Eq) {
                    self.expr()?
                } else {
                    // declaration without initializer — default it
                    Expr {
                        kind: ExprKind::Null,
                        span,
                    }
                };
                self.expect(&Tok::Semi)?;
                return Ok(Some(Stmt {
                    leading: Vec::new(),
                    kind: StmtKind::Local {
                        name,
                        ty: Some(ty),
                        mutable: true,
                        value,
                    },
                    span,
                }));
            }
        }
        self.i = save;
        Ok(None)
    }

    fn assign_or_expr(&mut self, span: Span) -> Result<Stmt, ParseError> {
        let lhs = self.expr()?;
        let op = match self.peek() {
            Tok::Eq => Some(AssignOp::Eq),
            Tok::PlusEq => Some(AssignOp::Add),
            Tok::MinusEq => Some(AssignOp::Sub),
            Tok::StarEq => Some(AssignOp::Mul),
            Tok::SlashEq => Some(AssignOp::Div),
            Tok::PercentEq => Some(AssignOp::Rem),
            Tok::QuestionQEq => Some(AssignOp::NullCoalesce),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let value = self.expr()?;
            return Ok(Stmt {
                leading: Vec::new(),
                kind: StmtKind::Assign {
                    target: lhs,
                    op,
                    value,
                },
                span,
            });
        }
        // `x++` / `x--` as statements → `x = x +/- 1`
        if self.peek() == &Tok::PlusPlus || self.peek() == &Tok::MinusMinus {
            let is_inc = self.peek() == &Tok::PlusPlus;
            self.bump();
            return Ok(Stmt {
                leading: Vec::new(),
                kind: StmtKind::Assign {
                    target: lhs,
                    op: if is_inc { AssignOp::Add } else { AssignOp::Sub },
                    value: Expr {
                        kind: ExprKind::Int(1),
                        span,
                    },
                },
                span,
            });
        }
        Ok(Stmt {
            leading: Vec::new(),
            kind: StmtKind::Expr(lhs),
            span,
        })
    }

    // ─── expressions ──────────────────────────────────────────────────────

    fn expr(&mut self) -> Result<Expr, ParseError> {
        self.range_expr()
    }

    fn range_expr(&mut self) -> Result<Expr, ParseError> {
        let lo = self.ternary()?;
        if self.peek() == &Tok::DotDot || self.peek() == &Tok::DotDotLt {
            let inclusive = self.peek() == &Tok::DotDot;
            let span = lo.span;
            self.bump();
            let hi = self.ternary()?;
            return Ok(Expr {
                kind: ExprKind::Range(Box::new(lo), Box::new(hi), inclusive),
                span,
            });
        }
        Ok(lo)
    }

    fn ternary(&mut self) -> Result<Expr, ParseError> {
        let c = self.coalesce()?;
        if self.peek() == &Tok::Question {
            let span = c.span;
            self.bump();
            let a = self.expr()?;
            self.expect(&Tok::Colon)?;
            let b = self.expr()?;
            return Ok(Expr {
                kind: ExprKind::Ternary(Box::new(c), Box::new(a), Box::new(b)),
                span,
            });
        }
        Ok(c)
    }

    fn coalesce(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.bin(0)?;
        while self.peek() == &Tok::QuestionQuestion {
            let span = lhs.span;
            self.bump();
            let rhs = self.bin(0)?;
            lhs = Expr {
                kind: ExprKind::NullCoalesce(Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    /// Precedence-climbing binary parser. Level 0 is `||`, climbing to `* / %`.
    fn bin(&mut self, min_level: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.unary()?;
        loop {
            let Some((op, level)) = bin_op(self.peek()) else {
                break;
            };
            if level < min_level {
                break;
            }
            let span = lhs.span;
            self.bump();
            let rhs = self.bin(level + 1)?;
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    /// `x => …`, `(a, b) => …`, `() => …`, `(int x) => …`. Returns `None` when
    /// the position does not start a lambda (the parser then backtracks).
    fn try_lambda(&mut self) -> Result<Option<Expr>, ParseError> {
        let span = self.span();
        // `x => …`
        if matches!(self.peek(), Tok::Ident(_)) && self.peek_at(1) == &Tok::FatArrow {
            let name = self.ident()?;
            self.expect(&Tok::FatArrow)?;
            let body = self.lambda_body()?;
            return Ok(Some(Expr {
                kind: ExprKind::Lambda(Lambda {
                    params: vec![(name, None)],
                    body,
                }),
                span,
            }));
        }
        if self.peek() != &Tok::LParen {
            return Ok(None);
        }
        let save = self.i;
        self.bump(); // (
        let mut params = Vec::new();
        let mut ok = true;
        while self.peek() != &Tok::RParen {
            // `T name` or `name`
            let before = self.i;
            let mut ty = None;
            if let Ok(t) = self.type_ref() {
                if matches!(self.peek(), Tok::Ident(_)) {
                    ty = Some(t);
                } else {
                    self.i = before;
                }
            } else {
                self.i = before;
            }
            match self.peek().clone() {
                Tok::Ident(n) => {
                    self.bump();
                    params.push((n, ty));
                }
                _ => {
                    ok = false;
                    break;
                }
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        if !ok || !self.eat(&Tok::RParen) || self.peek() != &Tok::FatArrow {
            self.i = save;
            return Ok(None);
        }
        self.expect(&Tok::FatArrow)?;
        let body = self.lambda_body()?;
        Ok(Some(Expr {
            kind: ExprKind::Lambda(Lambda { params, body }),
            span,
        }))
    }

    fn lambda_body(&mut self) -> Result<LambdaBody, ParseError> {
        if self.peek() == &Tok::LBrace {
            Ok(LambdaBody::Block(self.block()?))
        } else {
            Ok(LambdaBody::Expr(Box::new(self.expr()?)))
        }
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if let Some(l) = self.try_lambda()? {
            return Ok(l);
        }
        let span = self.span();
        let op = match self.peek() {
            Tok::Minus => Some(UnOp::Neg),
            Tok::Bang => Some(UnOp::Not),
            Tok::Tilde => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let e = self.unary()?;
            return Ok(Expr {
                kind: ExprKind::Unary(op, Box::new(e)),
                span,
            });
        }
        // cast: `(T) unary`
        if self.peek() == &Tok::LParen {
            if let Some(cast) = self.try_cast()? {
                return Ok(cast);
            }
        }
        self.postfix()
    }

    fn try_cast(&mut self) -> Result<Option<Expr>, ParseError> {
        let save = self.i;
        let span = self.span();
        self.bump(); // (
        let Ok(ty) = self.type_ref() else {
            self.i = save;
            return Ok(None);
        };
        if self.peek() != &Tok::RParen {
            self.i = save;
            return Ok(None);
        }
        // Only a cast if followed by something that begins a value.
        let next = self.peek_at(1);
        let looks_value = matches!(
            next,
            Tok::Ident(_)
                | Tok::Int(_, _)
                | Tok::Float(_, _)
                | Tok::Str(_)
                | Tok::InterpStr(_)
                | Tok::Char(_)
                | Tok::LParen
                | Tok::Keyword(Kw::New | Kw::This | Kw::True | Kw::False | Kw::Null)
                | Tok::Minus
                | Tok::Bang
                | Tok::Tilde
        );
        // A plain `(name)` where name is a value, not a type, is a paren expr.
        // Heuristic: treat as cast when the inner is a known primitive/generic
        // or an array/optional; otherwise only when clearly followed by a value
        // and not an operator context.
        if !looks_value {
            self.i = save;
            return Ok(None);
        }
        if !is_type_castable(&ty) {
            self.i = save;
            return Ok(None);
        }
        self.bump(); // )
        let e = self.unary()?;
        Ok(Some(Expr {
            kind: ExprKind::Cast(ty, Box::new(e)),
            span,
        }))
    }

    /// `subject switch { pat => value, … }`, if it follows.
    fn opt_switch(&mut self, subject: Expr) -> Result<Expr, ParseError> {
        if self.peek() != &Tok::Keyword(Kw::Switch) {
            return Ok(subject);
        }
        let span = subject.span;
        self.bump();
        self.expect(&Tok::LBrace)?;
        let mut arms = Vec::new();
        while self.peek() != &Tok::RBrace {
            let pat = if self.peek() == &Tok::Ident("_".into()) {
                self.bump();
                SwitchPat::Discard
            } else if let Some((op, _)) = relational_pat(self.peek()) {
                self.bump();
                SwitchPat::Relational(op, self.expr()?)
            } else {
                SwitchPat::Const(self.expr()?)
            };
            self.expect(&Tok::FatArrow)?;
            let value = self.expr()?;
            arms.push(SwitchArm { pat, value });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RBrace)?;
        Ok(Expr {
            kind: ExprKind::Switch(Box::new(subject), arms),
            span,
        })
    }

    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Tok::Dot => {
                    self.bump();
                    let name = self.ident()?;
                    e = Expr {
                        span: e.span,
                        kind: ExprKind::Member(Box::new(e), name),
                    };
                }
                Tok::LParen => {
                    let args = self.call_args()?;
                    e = Expr {
                        span: e.span,
                        kind: ExprKind::Call(Box::new(e), args),
                    };
                }
                Tok::LBracket => {
                    self.bump();
                    let idx = self.expr()?;
                    self.expect(&Tok::RBracket)?;
                    e = Expr {
                        span: e.span,
                        kind: ExprKind::Index(Box::new(e), Box::new(idx)),
                    };
                }
                Tok::Question if !matches!(self.peek_at(1), Tok::Colon) => {
                    // `e?` propagation — but not the ternary `?`. We only treat
                    // it as Try when followed by `;`, `)`, `,`, `.`, or end of a
                    // statement-ish context.
                    let follow = self.peek_at(1);
                    if matches!(
                        follow,
                        Tok::Semi | Tok::RParen | Tok::Comma | Tok::Dot | Tok::RBrace
                    ) {
                        self.bump();
                        e = Expr {
                            span: e.span,
                            kind: ExprKind::Try(Box::new(e)),
                        };
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        self.opt_switch(e)
    }

    fn call_args(&mut self) -> Result<Vec<Expr>, ParseError> {
        self.expect(&Tok::LParen)?;
        let mut out = Vec::new();
        while self.peek() != &Tok::RParen {
            // named argument `name: expr` — keep the value, drop the name for now
            if matches!(self.peek(), Tok::Ident(_)) && self.peek_at(1) == &Tok::Colon {
                self.bump();
                self.bump();
            }
            out.push(self.expr()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(out)
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        let span = self.span();
        let kind = match self.bump() {
            Tok::Int(v, _) => ExprKind::Int(v),
            Tok::Float(v, f32) => ExprKind::Float(v, f32),
            Tok::Str(s) => ExprKind::Str(s),
            Tok::Char(c) => ExprKind::Char(c),
            Tok::Keyword(Kw::True) => ExprKind::Bool(true),
            Tok::Keyword(Kw::False) => ExprKind::Bool(false),
            Tok::Keyword(Kw::Null) => ExprKind::Null,
            Tok::Keyword(Kw::This) => ExprKind::Ident("this".into()),
            Tok::Ident(s) => ExprKind::Ident(s),
            Tok::InterpStr(parts) => self.interp(parts)?,
            Tok::LParen => {
                let e = self.expr()?;
                self.expect(&Tok::RParen)?;
                return Ok(e);
            }
            Tok::Keyword(Kw::New) => return self.new_expr(span),
            // A collection expression: `[]` or `[a, b, c]`.
            Tok::LBracket => {
                let mut items = Vec::new();
                while self.peek() != &Tok::RBracket {
                    items.push(self.expr()?);
                    if self.peek() == &Tok::Comma {
                        self.bump();
                    } else {
                        break;
                    }
                }
                self.expect(&Tok::RBracket)?;
                ExprKind::Collection(items)
            }
            other => {
                self.i -= 1;
                return self.err(format!("expected an expression, found {other:?}"));
            }
        };
        Ok(Expr { kind, span })
    }

    fn interp(&mut self, parts: Vec<InterpPart>) -> Result<ExprKind, ParseError> {
        let mut segs = Vec::new();
        for part in parts {
            match part {
                InterpPart::Lit(s) => {
                    if !s.is_empty() {
                        segs.push(InterpSeg::Lit(s));
                    }
                }
                InterpPart::Hole(src) => {
                    // A format spec after `:` is dropped for now.
                    let expr_src = src.split(':').next().unwrap_or("").to_string();
                    let toks = crate::lexer::lex(&expr_src).map_err(|e| ParseError {
                        msg: e.msg,
                        span: self.span(),
                    })?;
                    let mut sub = Parser { toks, i: 0 };
                    let e = sub.expr()?;
                    segs.push(InterpSeg::Expr(Box::new(e)));
                }
            }
        }
        Ok(ExprKind::Interp(segs))
    }

    fn new_expr(&mut self, span: Span) -> Result<Expr, ParseError> {
        let ty = self.type_ref()?;
        let mut args = Vec::new();
        if self.peek() == &Tok::LParen {
            args = self.call_args()?;
        }
        let mut inits = Vec::new();
        if self.peek() == &Tok::LBrace {
            self.bump();
            while self.peek() != &Tok::RBrace {
                let fname = self.ident()?;
                self.expect(&Tok::Eq)?;
                let v = self.expr()?;
                inits.push((fname, v));
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RBrace)?;
        }
        Ok(Expr {
            kind: ExprKind::New(ty, args, inits),
            span,
        })
    }
}

/// A leading comparison in a pattern position: `> 0`, `<= 10`.
fn relational_pat(t: &Tok) -> Option<(BinOp, u8)> {
    Some(match t {
        Tok::Lt => (BinOp::Lt, 0),
        Tok::Le => (BinOp::Le, 0),
        Tok::Gt => (BinOp::Gt, 0),
        Tok::Ge => (BinOp::Ge, 0),
        _ => return None,
    })
}

fn bin_op(t: &Tok) -> Option<(BinOp, u8)> {
    Some(match t {
        Tok::PipePipe => (BinOp::Or, 0),
        Tok::AmpAmp => (BinOp::And, 1),
        Tok::Pipe => (BinOp::BitOr, 2),
        Tok::Caret => (BinOp::BitXor, 3),
        Tok::Amp => (BinOp::BitAnd, 4),
        Tok::EqEq => (BinOp::Eq, 5),
        Tok::Ne => (BinOp::Ne, 5),
        Tok::Lt => (BinOp::Lt, 6),
        Tok::Le => (BinOp::Le, 6),
        Tok::Gt => (BinOp::Gt, 6),
        Tok::Ge => (BinOp::Ge, 6),
        Tok::Shl => (BinOp::Shl, 7),
        Tok::Shr => (BinOp::Shr, 7),
        Tok::Plus => (BinOp::Add, 8),
        Tok::Minus => (BinOp::Sub, 8),
        Tok::Star => (BinOp::Mul, 9),
        Tok::Slash => (BinOp::Div, 9),
        Tok::Percent => (BinOp::Rem, 9),
        _ => return None,
    })
}

/// Whether a `(T)` should be read as a cast. Primitive names, generics, arrays
/// and optionals are; a bare capitalised name could be a value, so we accept it
/// too when the cast heuristic in `try_cast` already saw a value follow.
fn is_type_castable(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Named(n) => matches!(
            n.as_str(),
            "int"
                | "uint"
                | "long"
                | "ulong"
                | "short"
                | "ushort"
                | "byte"
                | "sbyte"
                | "nint"
                | "nuint"
                | "float"
                | "double"
                | "bool"
                | "char"
                | "string"
        ),
        TypeRef::Array(_) | TypeRef::Optional(_) | TypeRef::Generic(_, _) => true,
        TypeRef::Void => false,
    }
}
