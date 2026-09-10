//! `kiln lsp` — a Language Server Protocol server for `.kiln` sources.
//!
//! This is how Kiln gets a real editing experience without writing an editor
//! widget. Any LSP client (VS Code, Neovim, Helix, Zed — and, later, Studio's
//! own code pane) speaks to this over stdio and gets live diagnostics.
//!
//! Implemented: diagnostics, completion, signature help, hover,
//! go-to-definition, references and document symbols. Diagnostics came first
//! on purpose — a squiggle under the actual mistake is what makes an editor
//! usable, and getting it right forces the whole pipeline (framing, document
//! sync, position mapping) to be correct before anything is layered on top.
//!
//! Completion knows the one thing that makes a RAD tool a RAD tool: inside a
//! component block, `on ` offers that component's events, and the handler
//! position offers the subroutines that exist plus one that does not yet —
//! accepting it writes the `sub` with the event's parameter list at the end of
//! the file. Nobody should have to remember an event's name or its signature.
//!
//! Navigation is backed by `lsp_index`, a token-level index rather than an
//! AST one, so it keeps working on the half-typed files it will spend most of
//! its life looking at.
//!
//! Three design points worth stating:
//!
//! * **The registry is loaded once, at initialize.** Validation needs the
//!   command/component registry, which comes from introspecting support
//!   libraries — a subprocess and a `dlopen`. Doing that per keystroke would
//!   make typing lag. It is cached for the life of the server.
//! * **The server never exits on a bad workspace.** If the runtime can't be
//!   located, we still serve parse errors and report the degradation as a
//!   diagnostic. An editor plugin that dies on open is worse than one that
//!   does half the job.
//! * **Positions cross a unit boundary.** LSP counts columns in UTF-16 code
//!   units; the lexer counts bytes. They agree only for ASCII, so every
//!   position is converted at the edge rather than passed through.

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use lsp_server::{Connection, ExtractError, Message, Notification, Request, RequestId, Response};
use lsp_types::request::{
    Completion, DocumentSymbolRequest, GotoDefinition, HoverRequest, References,
    Request as _, SignatureHelpRequest,
};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, Diagnostic,
    DiagnosticSeverity, DocumentSymbolParams, Hover, HoverContents, HoverProviderCapability,
    InitializeParams, Location, MarkupContent, MarkupKind, OneOf, Position,
    ParameterInformation, ParameterLabel, PublishDiagnosticsParams, Range, ReferenceParams,
    ServerCapabilities, SignatureHelp, SignatureHelpOptions, SignatureInformation,
    SymbolInformation, SymbolKind, TextDocumentPositionParams, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Uri,
};

use kiln_ir::registry::ComponentDesc;
use kiln_ir::validate::{param_list, validate_unit, validate_with, Hints};
use kiln_ir::{parse, resolve_units, ParseOptions, Registry, Signature, Span};

use crate::kit;
use crate::libload;
use crate::lsp_index::{Index, Occurrence, SymKind};

/// Entry point for the `lsp` subcommand. Returns a process exit code.
pub fn run() -> i32 {
    // Diagnostics and logging go to stderr: stdout is the protocol channel and
    // a stray `println!` corrupts the stream.
    eprintln!("kiln-lsp: starting on stdio");
    match serve() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("kiln-lsp: {e}");
            1
        }
    }
}

fn serve() -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();

    let caps = serde_json::to_value(ServerCapabilities {
        // Full-text sync: `.kiln` files are small and we re-parse from scratch
        // anyway, so incremental sync would be complexity with no payoff.
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        // A client only sends requests for capabilities the server advertises,
        // so anything omitted here is dead code no matter how well it works.
        completion_provider: Some(CompletionOptions {
            // `.` opens the property/event list for a component; `:` on an
            // `on` line opens the handler list, where the item that creates
            // the subroutine lives — the one position a user has no name to
            // start typing at.
            trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        // Typing `(` is the moment you need to be told what goes inside it.
        // Without this the parameter list is only ever visible by hovering the
        // name you have already finished typing — which is the wrong moment.
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: Some(vec![",".to_string()]),
            ..Default::default()
        }),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        ..Default::default()
    })?;

    let init_params = connection.initialize(caps)?;
    let mut server = Server::new(serde_json::from_value(init_params).unwrap_or_default());

    let result = server.main_loop(&connection);

    // Drop the connection BEFORE joining. The writer thread lives until its
    // channel closes, and the channel closes when the last `Sender` — held by
    // `connection` — goes away. Joining first hangs the process forever, which
    // means every editor session would leak an `kiln lsp` behind it.
    drop(connection);
    io_threads.join()?;
    result?;
    eprintln!("kiln-lsp: shutting down");
    Ok(())
}

struct Server {
    /// Open documents, by URI. The client is the source of truth for content
    /// while a file is open — never read it back off disk.
    docs: HashMap<Uri, String>,
    /// Repo root that supplies the runtime, or `None` in degraded mode.
    repo_root: Option<PathBuf>,
    /// Cached registries, keyed by the module's `use` list. Loading is
    /// expensive; a module's imports rarely change between keystrokes.
    registries: HashMap<Vec<String>, Result<Registry, String>>,
    /// Command name -> the library that declares it, over every library the
    /// workspace can see. Built the first time a diagnostic needs it, since
    /// that means loading every kit's metadata, and most sessions never do.
    elsewhere: Option<HashMap<String, String>>,
}

impl Server {
    fn new(params: InitializeParams) -> Server {
        let workspace = workspace_root(&params);

        // Kit resolution finds a project's `kits/` by walking up from the
        // working directory, which for a language server is whatever the editor
        // happened to be launched from — often the user's home. Moving to the
        // workspace before any request makes the editor resolve kits the way
        // `kiln build` does in that project, which is the entire point: the
        // editor must not disagree with the compiler.
        if let Some(ws) = &workspace {
            let _ = std::env::set_current_dir(ws);
        }

        // Walking up from the editor's workspace finds the runtime when the
        // project lives inside a checkout. It does NOT when the project lives
        // anywhere else — which, for anything created from a template, is the
        // normal case. `KILN_RUNTIME_DIR` and our own location cover that:
        // the first is what an installed toolchain sets, the second is the
        // runtime shipping beside the binary.
        let repo_root = workspace
            .as_deref()
            .and_then(find_repo_root_from)
            .or_else(runtime_dir_from_env)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    .and_then(|d| find_repo_root_from(&d))
            });
        match &repo_root {
            Some(r) => eprintln!("kiln-lsp: runtime at {}", r.display()),
            None => eprintln!("kiln-lsp: no runtime found — parse-only mode"),
        }
        Server {
            docs: HashMap::new(),
            repo_root,
            registries: HashMap::new(),
            elsewhere: None,
        }
    }

    fn main_loop(&mut self, conn: &Connection) -> Result<(), Box<dyn Error + Sync + Send>> {
        for msg in &conn.receiver {
            match msg {
                Message::Request(req) => {
                    if conn.handle_shutdown(&req)? {
                        return Ok(());
                    }
                    self.on_request(conn, req);
                }
                Message::Notification(note) => self.on_notification(conn, note),
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    fn on_request(&mut self, conn: &Connection, req: Request) {
        let id: RequestId = req.id.clone();
        // Every request gets *some* response. A client that never hears back
        // about a request it sent waits forever.
        let result: Option<serde_json::Value> = match req.method.as_str() {
            Completion::METHOD => self.on_completion(req.params),
            HoverRequest::METHOD => self.on_hover(req.params),
            SignatureHelpRequest::METHOD => self.on_signature_help(req.params),
            GotoDefinition::METHOD => self.on_definition(req.params),
            References::METHOD => self.on_references(req.params),
            DocumentSymbolRequest::METHOD => self.on_document_symbol(req.params),
            _ => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::MethodNotFound as i32,
                    format!("kiln-lsp does not support `{}` yet", req.method),
                );
                let _ = conn.sender.send(Message::Response(resp));
                return;
            }
        };
        // `null` is a valid, meaningful answer: "nothing here".
        let resp = Response::new_ok(id, result.unwrap_or(serde_json::Value::Null));
        let _ = conn.sender.send(Message::Response(resp));
    }

    /// Source text and cursor for a positional request, or `None` if we don't
    /// have the document.
    fn context(&self, params: serde_json::Value) -> Option<(String, usize, usize)> {
        let p: TextDocumentPositionParams = serde_json::from_value(params).ok()?;
        let src = self.docs.get(&p.text_document.uri)?.clone();
        let line = p.position.line as usize + 1; // LSP is 0-based, we are 1-based
        let col = utf16_col_to_byte(&src, line, p.position.character);
        Some((src, line, col))
    }

    fn on_completion(&mut self, params: serde_json::Value) -> Option<serde_json::Value> {
        let p: CompletionParams = serde_json::from_value(params.clone()).ok()?;
        let src = self
            .docs
            .get(&p.text_document_position.text_document.uri)?
            .clone();
        let line_no = p.text_document_position.position.line as usize + 1;
        let col = utf16_col_to_byte(&src, line_no, p.text_document_position.position.character);
        let line_text = nth_line(&src, line_no).unwrap_or_default();
        // Text to the left of the caret. `col` is a 1-based byte column.
        let cut = col.saturating_sub(1).min(line_text.len());
        let before = &line_text[..cut];

        let ix = Index::build(&src);
        let mut items: Vec<CompletionItem> = Vec::new();

        // On a `use` line the only legal words are library names, and they are
        // exactly the hardest thing to guess: a kit installed under `~` or
        // shipped in `kits/` appears nowhere in the file you are editing.
        if before.trim_start().strip_prefix("use").is_some_and(|r| {
            r.starts_with(char::is_whitespace) && !r.trim().contains(char::is_whitespace)
        }) {
            if let Some(root) = self.repo_root.clone() {
                for k in kit::resolve_all(&root) {
                    items.push(item(
                        &k.name,
                        CompletionItemKind::MODULE,
                        format!("{} — {} {}", k.display, k.tier.as_str(), k.version),
                    ));
                }
            }
            return serde_json::to_value(items).ok();
        }

        let registry = self.registry_for_src(&ix);

        // Inside a component block, an `on` line is the RAD loop: the event
        // names are the component's, and the handler is a subroutine that may
        // not exist yet. Both halves are answered from the descriptor, so
        // neither has to be remembered.
        if let Some((event, handler)) = on_line(before) {
            let block = ix.block_at_line(line_no);
            let desc = block
                .zip(registry.as_ref())
                .and_then(|(b, reg)| reg.component(&b.type_name));
            let (Some(block), Some(desc), Some(reg)) = (block, desc, registry.as_ref()) else {
                return serde_json::to_value(items).ok();
            };
            match handler {
                None => {
                    for ev in &desc.events {
                        let hands = param_list(reg.event_params(&desc.name, ev));
                        items.push(item(
                            ev,
                            CompletionItemKind::EVENT,
                            if hands.is_empty() {
                                format!("{} event", desc.name)
                            } else {
                                format!("{} event — hands ({hands})", desc.name)
                            },
                        ));
                    }
                }
                Some(_) => {
                    let subs: Vec<&str> = ix
                        .names_in_scope(None)
                        .into_iter()
                        .filter(|(_, k)| *k == SymKind::Sub)
                        .map(|(n, _)| n)
                        .collect();
                    for name in &subs {
                        items.push(item(name, CompletionItemKind::FUNCTION, "subroutine".into()));
                    }
                    // The one that does not exist: named after the component
                    // and the event, declared with what the event hands over,
                    // and written at the end of the file where a new
                    // subroutine goes. Offered only while there is no such
                    // subroutine — once there is, it is in the list above.
                    let name = format!("{}_{}", block.id, event);
                    if desc.has_event(&event) && !subs.contains(&name.as_str()) {
                        let params = param_list(reg.event_params(&desc.name, &event));
                        items.push(new_handler_item(&src, &name, &params));
                    }
                }
            }
            return serde_json::to_value(items).ok();
        }

        // After `id.` the only sensible completions are that component's
        // properties and events — offering variables there would be noise.
        if let Some(id) = trailing_member_target(before) {
            if let (Some(ty), Some(reg)) = (ix.component_types.get(id), registry.as_ref()) {
                if let Some(desc) = reg.component(ty) {
                    for prop in &desc.properties {
                        items.push(item(
                            &prop.name,
                            CompletionItemKind::PROPERTY,
                            format!("{}: {}", ty, prop.ty.as_str()),
                        ));
                    }
                    for ev in &desc.events {
                        items.push(item(
                            ev,
                            CompletionItemKind::EVENT,
                            format!("{ty} event"),
                        ));
                    }
                }
            }
            return serde_json::to_value(items).ok();
        }

        if let Some(reg) = &registry {
            for (name, cmd) in reg.iter() {
                items.push(item(
                    name,
                    CompletionItemKind::FUNCTION,
                    signature_text(name, &cmd.sig),
                ));
            }
            for name in reg.component_names() {
                items.push(item(name, CompletionItemKind::CLASS, "component".into()));
            }
            // Foreign functions, record types and constants a kit's `.kdecl`
            // bundle contributed. They are in the registry the same way a
            // command is — the file that `use`s the kit never spells them out —
            // so completion has to read them from here, not from the file's own
            // index. A `dll` is called like a function; a record is a type; a
            // constant stands for its literal.
            for name in reg.dll_names() {
                let detail = reg
                    .dll(name)
                    .map(|d| signature_text(name, &d.sig))
                    .unwrap_or_else(|| "foreign function".into());
                items.push(item(name, CompletionItemKind::FUNCTION, detail));
            }
            for name in reg.record_names() {
                items.push(item(name, CompletionItemKind::STRUCT, "record type".into()));
            }
            for name in reg.const_names() {
                let detail = reg
                    .const_(name)
                    .map(|c| format!("constant: {}", c.ty.as_str()))
                    .unwrap_or_else(|| "constant".into());
                items.push(item(name, CompletionItemKind::CONSTANT, detail));
            }
        }
        for (name, kind) in ix.names_in_scope(ix.scope_at_line(line_no)) {
            let (k, detail) = match kind {
                SymKind::Sub => (CompletionItemKind::FUNCTION, "subroutine"),
                SymKind::Global => (CompletionItemKind::VARIABLE, "module variable"),
                SymKind::Local => (CompletionItemKind::VARIABLE, "local"),
                SymKind::Component => (CompletionItemKind::FIELD, "component"),
                SymKind::Record => (CompletionItemKind::STRUCT, "record type"),
                _ => (CompletionItemKind::TEXT, ""),
            };
            items.push(item(name, k, detail.to_string()));
        }
        for kw in KEYWORDS {
            items.push(item(kw, CompletionItemKind::KEYWORD, "keyword".into()));
        }
        serde_json::to_value(items).ok()
    }

    fn on_hover(&mut self, params: serde_json::Value) -> Option<serde_json::Value> {
        let (src, line, col) = self.context(params)?;
        let ix = Index::build(&src);
        let occ = ix.at(line, col)?;
        let registry = self.registry_for_src(&ix);

        // A property or event of a component, either as `id.name` or as a
        // line inside the component's own block.
        let member = registry.as_ref().and_then(|reg| {
            let (id, ty) = if occ.kind == SymKind::Property {
                let id = ix
                    .occurrences
                    .iter()
                    .find(|o| o.line == occ.line && o.end_col() + 1 == occ.col)?;
                (id.name.clone(), ix.component_types.get(&id.name)?.clone())
            } else {
                let b = ix.block_at_line(occ.line)?;
                (b.id.clone(), b.type_name.clone())
            };
            let desc = reg.component(&ty)?;
            member_hover(reg, desc, &id, occ, &nth_line(&src, occ.line).unwrap_or_default())
        });

        let text = if let Some(text) = member {
            text
        } else if let Some(cmd) = registry.as_ref().and_then(|r| r.get(&occ.name)) {
            let sig = signature_text(&occ.name, &cmd.sig);
            let doc = registry.as_ref().and_then(|r| r.doc(&occ.name));
            // The signature says what the command takes. The sentence says what
            // it is for, and the example says what calling it looks like —
            // which is the question someone hovering a name is actually asking.
            // A command with neither reads exactly as it did before.
            let mut text = match doc.map(|d| d.summary.as_str()).unwrap_or("") {
                "" => format!("```\n{sig}\n```\n\ncommand"),
                summary => format!("```\n{sig}\n```\n\n{summary}."),
            };
            if let Some(example) = doc.map(|d| d.example.as_str()).filter(|e| !e.is_empty()) {
                // `kiln` is the language id the VS Code extension registers,
                // so the block highlights rather than sitting there grey.
                text.push_str(&format!("\n\n```kiln\n{example}\n```"));
            }
            text
        } else if let Some(ty) = ix.component_types.get(&occ.name) {
            format!("```\n{} {}\n```\n\ncomponent", ty, occ.name)
        } else if let Some(header) = ix.sub_headers.get(&occ.name) {
            format!("```\n{}{header}\n```\n\nsubroutine", occ.name)
        } else if let Some(def) = ix.definition_of(occ) {
            format!(
                "```\n{}\n```\n\n{} — declared on line {}",
                occ.name,
                describe(def.kind),
                def.line
            )
        } else {
            return None;
        };
        // A `##` note above the declaration is the author's own account of the
        // symbol, and it goes below the signature — which says what the thing
        // takes, never what it is for. A plain `#` comment is not shown: that
        // is the whole difference between the two spellings.
        let text = match ix.docs.get(&occ.name) {
            Some(doc) => format!("{text}\n\n{doc}"),
            None => text,
        };

        serde_json::to_value(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: text,
            }),
            range: Some(occ_range(&src, occ)),
        })
        .ok()
    }

    /// What goes inside the parentheses you are typing.
    ///
    /// Answers for commands and for the module's own subroutines. A
    /// subroutine's parameter list is rendered from the source line that
    /// declares it rather than from types, so it carries the author's own
    /// parameter *names* — `to: text` says more than `text` ever can.
    fn on_signature_help(&mut self, params: serde_json::Value) -> Option<serde_json::Value> {
        let (src, line, col) = self.context(params)?;
        let line_text = nth_line(&src, line).unwrap_or_default();
        let cut = col.saturating_sub(1).min(line_text.len());
        let (name, active) = enclosing_call(&line_text[..cut])?;

        let ix = Index::build(&src);
        let label = match ix.sub_headers.get(&name) {
            Some(header) => format!("{name}{header}"),
            None => {
                let reg = self.registry_for_src(&ix)?;
                signature_text(&name, &reg.get(&name)?.sig)
            }
        };

        let params_info: Vec<ParameterInformation> = parameter_labels(&label)
            .into_iter()
            .map(|p| ParameterInformation {
                label: ParameterLabel::Simple(p),
                documentation: None,
            })
            .collect();
        // A call with more arguments than parameters is a call being edited
        // wrongly; clamping keeps the client highlighting *something* rather
        // than dropping the popup at the moment the mistake is made.
        let active_parameter = params_info
            .len()
            .saturating_sub(1)
            .min(active)
            .try_into()
            .ok();

        serde_json::to_value(SignatureHelp {
            signatures: vec![SignatureInformation {
                label,
                documentation: None,
                parameters: Some(params_info),
                active_parameter,
            }],
            active_signature: Some(0),
            active_parameter,
        })
        .ok()
    }

    fn on_definition(&mut self, params: serde_json::Value) -> Option<serde_json::Value> {
        let p: TextDocumentPositionParams = serde_json::from_value(params.clone()).ok()?;
        let uri = p.text_document.uri.clone();
        let (src, line, col) = self.context(params)?;
        let ix = Index::build(&src);
        let occ = ix.at(line, col)?;
        // Commands live in C support libraries: there is no `.kiln` position to
        // jump to. Returning null is honest; hover carries the signature.
        let def = ix.definition_of(occ)?;
        serde_json::to_value(Location {
            uri,
            range: occ_range(&src, def),
        })
        .ok()
    }

    fn on_references(&mut self, params: serde_json::Value) -> Option<serde_json::Value> {
        let p: ReferenceParams = serde_json::from_value(params.clone()).ok()?;
        let uri = p.text_document_position.text_document.uri.clone();
        let src = self.docs.get(&uri)?.clone();
        let line = p.text_document_position.position.line as usize + 1;
        let col = utf16_col_to_byte(&src, line, p.text_document_position.position.character);

        let ix = Index::build(&src);
        let occ = ix.at(line, col)?;
        let include_decl = p.context.include_declaration;
        let locs: Vec<Location> = ix
            .references_to(occ)
            .into_iter()
            .filter(|o| include_decl || !o.is_definition)
            .map(|o| Location {
                uri: uri.clone(),
                range: occ_range(&src, o),
            })
            .collect();
        serde_json::to_value(locs).ok()
    }

    /// Powers the editor's outline / breadcrumb view.
    fn on_document_symbol(&mut self, params: serde_json::Value) -> Option<serde_json::Value> {
        let p: DocumentSymbolParams = serde_json::from_value(params).ok()?;
        let src = self.docs.get(&p.text_document.uri)?.clone();
        let ix = Index::build(&src);
        let syms: Vec<SymbolInformation> = ix
            .occurrences
            .iter()
            .filter(|o| o.is_definition && o.scope.is_none())
            .map(|o| {
                #[allow(deprecated)]
                SymbolInformation {
                    name: o.name.clone(),
                    kind: match o.kind {
                        SymKind::Sub => SymbolKind::FUNCTION,
                        SymKind::Component => SymbolKind::FIELD,
                        SymKind::Record => SymbolKind::STRUCT,
                        _ => SymbolKind::VARIABLE,
                    },
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: p.text_document.uri.clone(),
                        range: occ_range(&src, o),
                    },
                    container_name: None,
                }
            })
            .collect();
        serde_json::to_value(syms).ok()
    }

    /// Registry for a document as it is being typed.
    ///
    /// The `use` lines come from the index, not the parser. The parser refuses
    /// the file at exactly the moments completion and hover are asked — a
    /// line that reads `on ` is a parse error — and a registry built without
    /// the file's libraries knows the core `timer` but not the `ui` form and
    /// button that `on ` is nearly always typed in.
    fn registry_for_src(&mut self, ix: &Index) -> Option<Registry> {
        self.registry_for(&ix.uses).ok()
    }

    fn on_notification(&mut self, conn: &Connection, note: Notification) {
        match note.method.as_str() {
            DidOpenTextDocument::METHOD => {
                if let Ok(p) = cast_note::<DidOpenTextDocument>(note) {
                    let uri = p.text_document.uri.clone();
                    self.docs.insert(uri.clone(), p.text_document.text);
                    self.publish(conn, &uri);
                }
            }
            DidChangeTextDocument::METHOD => {
                if let Ok(p) = cast_note::<DidChangeTextDocument>(note) {
                    // FULL sync: the last change carries the whole document.
                    if let Some(change) = p.content_changes.into_iter().next_back() {
                        let uri = p.text_document.uri.clone();
                        self.docs.insert(uri.clone(), change.text);
                        self.publish(conn, &uri);
                    }
                }
            }
            DidSaveTextDocument::METHOD => {
                if let Ok(p) = cast_note::<DidSaveTextDocument>(note) {
                    self.publish(conn, &p.text_document.uri);
                }
            }
            DidCloseTextDocument::METHOD => {
                if let Ok(p) = cast_note::<DidCloseTextDocument>(note) {
                    let uri = p.text_document.uri;
                    self.docs.remove(&uri);
                    // Clear the squiggles: stale diagnostics on a closed file
                    // linger in the client's problem list otherwise.
                    send_diagnostics(conn, &uri, Vec::new());
                }
            }
            _ => {}
        }
    }

    fn publish(&mut self, conn: &Connection, uri: &Uri) {
        let Some(src) = self.docs.get(uri).cloned() else {
            return;
        };
        let dir = uri_to_path(uri).and_then(|p| p.parent().map(Path::to_path_buf));
        send_diagnostics(conn, uri, self.diagnose(&src, dir.as_deref()));
    }

    /// Compile far enough to collect diagnostics, and no further.
    ///
    /// `dir` is where the file lives, so its `use` names can resolve to unit
    /// files beside it; `None` (an untitled buffer) means every `use` is a
    /// library. A unit is checked as a unit — its own `use`s merged, no entry
    /// point demanded — and a program has its units merged before it is
    /// checked, exactly as `kiln build` sees it. The one thing not shown here
    /// is an error *inside* a unit while the program is the open file: the
    /// validator's line numbers are the unit's, and they would land on the
    /// wrong text. Those are counted, and the count points at the unit.
    fn diagnose(&mut self, src: &str, dir: Option<&Path>) -> Vec<Diagnostic> {
        let parsed = match parse(src) {
            Ok(m) => m,
            // A parse error stops everything downstream — while you are typing,
            // this is the common case, so it must be positioned well.
            Err(e) => return vec![diag(src, Span::line(e.line), e.msg)],
        };
        let is_unit = parsed.is_unit;
        let (module, origins) = match dir {
            Some(d) => match resolve_units(parsed, d, ParseOptions::default()) {
                Ok(r) => (r.module, r.origins),
                Err(e) => return vec![diag(src, Span::line(1), e.to_string())],
            },
            None => (parsed, Default::default()),
        };

        let registry = match self.registry_for(&module.uses) {
            Ok(r) => r,
            Err(msg) => {
                // Degraded: we parsed, but can't type-check. Say so once, at the
                // top of the file, instead of pretending the file is clean.
                return vec![diag(src, Span::line(1), format!("Kiln runtime unavailable: {msg}"))];
            }
        };

        let check = |m, r, h: &Hints| if is_unit { validate_unit(m, r, h) } else { validate_with(m, r, h) };
        let mut errs = match check(&module, &registry, &Hints::default()) {
            Ok(()) => return Vec::new(),
            Err(errs) => errs,
        };
        // An unknown command is the one diagnostic that gets better with what
        // the other libraries know, and the only one worth loading them for.
        if errs.iter().any(|e| e.msg.contains("unknown command `")) {
            let hints = Hints {
                elsewhere: self.elsewhere().clone(),
            };
            if let Err(better) = check(&module, &registry, &hints) {
                errs = better;
            }
        }
        // Which file an error is in: the subroutine the validator says it is
        // inside, looked up among what the units declared.
        let unit_of = |msg: &str| -> Option<PathBuf> {
            let name = msg.strip_prefix("in `")?.split_once('`')?.0;
            origins.get(name).cloned()
        };
        let mut here = Vec::new();
        let mut elsewhere: std::collections::BTreeMap<PathBuf, usize> = Default::default();
        for e in errs {
            match unit_of(&e.msg) {
                Some(file) => *elsewhere.entry(file).or_insert(0) += 1,
                // `msg`, not `to_string()`: Display prefixes "line N:", which
                // the editor already shows via the range.
                None => here.push(diag(src, e.span(), e.msg)),
            }
        }
        for (file, n) in elsewhere {
            let name = file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            here.push(diag(
                src,
                Span::line(1),
                format!("unit {name} has {n} error(s) — open it to see them"),
            ));
        }
        here
    }

    /// Every command of every library the workspace can see, and which
    /// library it is in. Loaded one kit at a time — two libraries can
    /// legitimately collide, and one registry holding both would refuse.
    fn elsewhere(&mut self) -> &HashMap<String, String> {
        if self.elsewhere.is_none() {
            let mut map = HashMap::new();
            let kits = self.repo_root.clone().map(|r| kit::resolve_all(&r)).unwrap_or_default();
            for k in kits {
                let uses = vec![k.name.clone()];
                if let Ok(reg) = self.registry_for(&uses) {
                    for (name, _) in reg.iter() {
                        map.entry(name.to_string()).or_insert_with(|| k.name.clone());
                    }
                }
            }
            // Core's commands are in every registry and never "elsewhere".
            for name in Registry::core().names() {
                map.remove(name);
            }
            self.elsewhere = Some(map);
        }
        self.elsewhere.as_ref().unwrap()
    }

    fn registry_for(&mut self, uses: &[String]) -> Result<Registry, String> {
        let key = uses.to_vec();
        if let Some(cached) = self.registries.get(&key) {
            return cached.clone();
        }
        let result = match &self.repo_root {
            None => Err("could not locate runtime/kiln_core.h from the workspace root".into()),
            // Through the same kit overlay `build` uses, and metadata-only for
            // the same reason `kiln commands` is: a library's commands are
            // readable without the ability to *link* it, so a project that uses
            // the UI stack still completes on a machine that never vendored it.
            // Resolving differently from the compiler is the worst failure this
            // server has — the editor would underline code that builds.
            Some(root) => kit::overlay_root(root, uses)
                .and_then(|staged| libload::load_metadata(&staged, uses))
                .map(|p| p.registry),
        };
        self.registries.insert(key, result.clone());
        result
    }
}

/// Where to start looking for the runtime. Prefer the client's workspace,
/// falling back to our own working directory.
fn workspace_root(params: &InitializeParams) -> Option<PathBuf> {
    #[allow(deprecated)]
    if let Some(folders) = &params.workspace_folders {
        if let Some(f) = folders.first() {
            if let Some(p) = uri_to_path(&f.uri) {
                return Some(p);
            }
        }
    }
    #[allow(deprecated)]
    if let Some(root) = &params.root_uri {
        if let Some(p) = uri_to_path(root) {
            return Some(p);
        }
    }
    std::env::current_dir().ok()
}

/// Turn a `file:` URI into a path.
///
/// `lsp-types` 0.97 exposes a bare RFC-3986 `Uri` with no filesystem mapping,
/// so we do the two things that actually matter: require the `file` scheme and
/// percent-decode the path. Anything else (a URI from a virtual filesystem) is
/// not something we can locate a runtime relative to.
fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    if uri.scheme().map(|s| s.as_str().to_ascii_lowercase()) != Some("file".into()) {
        return None;
    }
    let raw = uri.path().as_str();
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// The repo root implied by `$KILN_RUNTIME_DIR`, which an installed
/// toolchain sets and a checkout does not. Checked only after the workspace,
/// so a checkout you are editing still wins over an installation elsewhere.
fn runtime_dir_from_env() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("KILN_RUNTIME_DIR")?);
    dir.join("kiln_core.h")
        .is_file()
        .then(|| dir.parent().map(Path::to_path_buf))?
}

/// Walk up from `start` looking for `runtime/kiln_core.h`, mirroring the
/// build path's search but rooted at the editor's workspace rather than at CWD.
fn find_repo_root_from(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join("runtime/kiln_core.h").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Keywords offered in completion. Small enough to spell out; keeping it here
/// rather than deriving it from the lexer means completion can't accidentally
/// offer internal tokens like `Eof`.
const KEYWORDS: &[&str] = &[
    "module", "sub", "end", "let", "var", "call", "return", "if", "else", "while", "and", "or",
    "not",
    "true", "false", "use", "form", "on", "int", "int64", "double", "bool", "text", "bytes", "ptr",
    // Loops. `for`, `break` and `continue` are reserved; `to` and `step` are
    // soft keywords, offered here but usable as names elsewhere.
    "for", "break", "continue", "to", "step",
    // Build targets. `target` is a soft keyword, so it is offered
    // here but not reserved by the lexer.
    "target", "console", "gui", "sharedlib", "staticlib",
    // `through` makes a `call` an indirect one. It is a soft keyword — it means
    // that only straight after `call` — so it is offered and reserves nothing.
    "through",
    // Bitwise operators. The infix ones are soft keywords — they mean the
    // operator only in operator position — so they are offered but reserve
    // nothing; `bnot` is reserved by the lexer, like `not`.
    "band", "bor", "bxor", "bnot", "shl", "shr", "ushr",
    // The 0.8.0 shorthands. All are soft keywords the parser recognises by
    // position — none is reserved by the lexer, so a variable may still be
    // named for any of them. `increment`/`decrement` lead a statement; `in`
    // does double duty (a membership test `e in xs`, and a range/`for each`
    // bound); `each` and `at` belong to `for each x at i in xs`.
    "increment", "decrement", "in", "each", "at",
    // The 0.9.0 expression sugar, soft keywords every one. `then` joins an `if`
    // used as a *value* to its first arm; `otherwise` supplies the value a
    // failed call did not; `check` leads a statement or a `let` initializer and
    // returns early when the call it guards failed.
    "then", "otherwise", "check",
    // The 0.9.0 control-flow and declaration sugar, soft keywords every one.
    // `match`/`when` is the if-chain that tests one value; `repeat N times` is
    // the counting loop with its counter hidden; `assert` is the check a
    // release build compiles out; `enum` names a run of ints from 1.
    "match", "when", "repeat", "times", "assert", "enum",
    // The 0.9.0 hard tier. `some` and `as` open an optional (`if some v as
    // value`) and are soft keywords claimed only there; `where` filters a list
    // built by a loop (`[e for each x in xs where c]`); `defer` runs a statement
    // when its block ends. `none` is the one *literal* among them — a bare
    // `none` is the empty optional wherever it is written, so it is not usable
    // as a name.
    "some", "as", "where", "defer", "none",
];

fn item(label: &str, kind: CompletionItemKind, detail: String) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        detail: if detail.is_empty() { None } else { Some(detail) },
        ..Default::default()
    }
}

fn describe(kind: SymKind) -> &'static str {
    match kind {
        SymKind::Sub => "subroutine",
        SymKind::Global => "module variable",
        SymKind::Local => "local variable",
        SymKind::Component => "component",
        SymKind::ComponentType => "component type",
        SymKind::Record => "record type",
        SymKind::Command => "command",
        SymKind::Property => "property",
    }
}

fn signature_text(name: &str, sig: &Signature) -> String {
    let params = sig
        .params
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    match sig.ret {
        Some(r) => format!("{name}({params}) -> {}", r.as_str()),
        None => format!("{name}({params})"),
    }
}

/// The component id in `... id.partial` to the left of the caret, if the user
/// is completing a member.
///
/// The caret is normally *inside* the member name, not glued to the dot: you
/// type `ok.te` and still expect `text`. So skip back over the partial word
/// first, then require the dot.
fn trailing_member_target(before: &str) -> Option<&str> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let stem = before.trim_end_matches(is_word).strip_suffix('.')?;
    let start = stem.rfind(|c: char| !is_word(c)).map_or(0, |i| i + 1);
    let id = &stem[start..];
    (!id.is_empty()).then_some(id)
}

/// An `on` line to the left of the caret: the event name typed so far, and
/// the handler name typed so far once the `:` is there.
///
/// `on cl` is `("cl", None)`; `on click: ok_` is `("click", Some("ok_"))`.
/// A line that is not an `on` line is `None`, which is every other line.
fn on_line(before: &str) -> Option<(String, Option<String>)> {
    let rest = before.trim_start().strip_prefix("on")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    match rest.split_once(':') {
        None => rest.chars().all(is_word).then(|| (rest.to_string(), None)),
        Some((event, handler)) => {
            let event = event.trim();
            let handler = handler.trim_start();
            (event.chars().all(is_word) && handler.chars().all(is_word))
                .then(|| (event.to_string(), Some(handler.to_string())))
        }
    }
}

/// The completion that creates a handler: the name goes at the caret, the
/// subroutine goes at the end of the file.
fn new_handler_item(src: &str, name: &str, params: &str) -> CompletionItem {
    let header = if params.is_empty() {
        format!("sub {name}")
    } else {
        format!("sub {name}({params})")
    };
    // Appended after the last line, on a line of its own even when the file
    // does not end with a newline yet.
    let last_line = src.lines().count() as u32;
    let lead = if src.is_empty() || src.ends_with('\n') { "\n" } else { "\n\n" };
    CompletionItem {
        label: name.to_string(),
        kind: Some(CompletionItemKind::FUNCTION),
        detail: Some(format!("new subroutine — writes `{header}` at the end of the file")),
        insert_text: Some(name.to_string()),
        // First in the list: it is the thing an `on` line is usually for.
        sort_text: Some("0".into()),
        preselect: Some(true),
        additional_text_edits: Some(vec![TextEdit {
            range: Range {
                start: Position::new(last_line, 0),
                end: Position::new(last_line, 0),
            },
            new_text: format!("{lead}{header}\n  \nend\n"),
        }]),
        ..Default::default()
    }
}

/// Hover text for a property or event of component `id`, or `None` when the
/// name is neither.
///
/// A property shows its type and the editor an inspector would offer for it;
/// an event shows what it hands a handler. The property's default is not
/// carried by the registry yet.
fn member_hover(
    reg: &Registry,
    desc: &ComponentDesc,
    id: &str,
    occ: &Occurrence,
    line_text: &str,
) -> Option<String> {
    let on_an_event_line = line_text.trim_start().starts_with("on ");
    if on_an_event_line && desc.has_event(&occ.name) {
        let hands = param_list(reg.event_params(&desc.name, &occ.name));
        let header = if hands.is_empty() {
            "sub handler".to_string()
        } else {
            format!("sub handler({hands})")
        };
        return Some(format!(
            "```\n{}.{}\n```\n\nevent of `{id}` — a handler is `{header}`",
            desc.name, occ.name
        ));
    }
    let prop = desc.property(&occ.name)?;
    let editor = if prop.editor.is_empty() {
        String::new()
    } else {
        format!(" — editor: {}", prop.editor)
    };
    Some(format!(
        "```\n{}.{}: {}\n```\n\nproperty of `{id}`{editor}",
        desc.name,
        prop.name,
        prop.ty.as_str()
    ))
}

/// The call the caret is inside: the callee's name and the 0-based index of
/// the argument being typed.
///
/// Scanned right-to-left over the text before the caret, because that is the
/// only part of the line that is reliably complete — the closing parenthesis
/// usually has not been typed yet, and requiring one would mean signature help
/// appeared only after it was no longer needed. String literals are skipped so
/// that a `(` or `,` inside `"a, b"` does not move the highlight.
fn enclosing_call(before: &str) -> Option<(String, usize)> {
    let bytes = before.as_bytes();
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut i = bytes.len();
    // Whether the tail is inside a string is only knowable from the left, so
    // the quote parity of the whole prefix is computed once, then consumed
    // from the right along with everything else.
    let mut in_string = before.bytes().filter(|b| *b == b'"').count() % 2 == 1;
    while i > 0 {
        i -= 1;
        let c = bytes[i];
        if c == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match c {
            b')' => depth += 1,
            b'(' if depth > 0 => depth -= 1,
            b'(' => {
                let stem = &before[..i];
                let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
                let stem = stem.trim_end();
                let start = stem.rfind(|ch: char| !is_word(ch)).map_or(0, |j| j + 1);
                let name = &stem[start..];
                return (!name.is_empty()).then(|| (name.to_string(), commas));
            }
            b',' if depth == 0 => commas += 1,
            _ => {}
        }
    }
    None
}

/// Split a rendered signature `name(a: int, b: text) -> int` into its
/// parameter labels, so the client can highlight the one being typed.
fn parameter_labels(label: &str) -> Vec<String> {
    let Some(open) = label.find('(') else {
        return Vec::new();
    };
    let rest = &label[open + 1..];
    let Some(close) = rest.rfind(')') else {
        return Vec::new();
    };
    rest[..close]
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

fn nth_line(src: &str, line_1based: usize) -> Option<String> {
    src.lines().nth(line_1based.checked_sub(1)?).map(str::to_string)
}

/// Convert an LSP character offset (UTF-16 code units, 0-based) to our 1-based
/// **byte** column.
///
/// LSP measures columns in UTF-16 code units, our lexer in bytes. They agree
/// only for ASCII — so a file containing `"héllo"` would silently misplace
/// every cursor to its right if we skipped this.
fn utf16_col_to_byte(src: &str, line_1based: usize, character: u32) -> usize {
    let Some(line) = nth_line(src, line_1based) else {
        return 1;
    };
    let mut units = 0u32;
    for (byte_idx, ch) in line.char_indices() {
        if units >= character {
            return byte_idx + 1;
        }
        units += ch.len_utf16() as u32;
    }
    line.len() + 1
}

/// Convert our 1-based byte column to an LSP character offset.
fn byte_col_to_utf16(src: &str, line_1based: usize, byte_col: usize) -> u32 {
    let Some(line) = nth_line(src, line_1based) else {
        return 0;
    };
    let upto = byte_col.saturating_sub(1).min(line.len());
    line[..upto].chars().map(|c| c.len_utf16() as u32).sum()
}

/// The LSP range covering one identifier occurrence.
fn occ_range(src: &str, occ: &Occurrence) -> Range {
    let line = occ.line.saturating_sub(1) as u32;
    Range {
        start: Position::new(line, byte_col_to_utf16(src, occ.line, occ.col)),
        end: Position::new(line, byte_col_to_utf16(src, occ.line, occ.end_col())),
    }
}

/// Build a diagnostic at `at`: under the name when the validator knows one,
/// across the whole line when it only knows the line.
fn diag(src: &str, at: Span, msg: String) -> Diagnostic {
    // LSP lines are 0-based; ours are 1-based, and 0 means "unknown".
    let line = at.line.saturating_sub(1) as u32;
    let range = if at.col > 0 && at.end_col > at.col {
        Range {
            start: Position::new(line, byte_col_to_utf16(src, at.line, at.col)),
            end: Position::new(line, byte_col_to_utf16(src, at.line, at.end_col)),
        }
    } else {
        Range {
            start: Position::new(line, 0),
            // u32::MAX is clamped by the client to the real end of the line.
            end: Position::new(line, u32::MAX),
        }
    };
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("kiln".into()),
        message: msg,
        ..Default::default()
    }
}

fn send_diagnostics(conn: &Connection, uri: &Uri, diagnostics: Vec<Diagnostic>) {
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics,
        version: None,
    };
    if let Ok(params) = serde_json::to_value(params) {
        let _ = conn.sender.send(Message::Notification(Notification {
            method: PublishDiagnostics::METHOD.to_string(),
            params,
        }));
    }
}

fn cast_note<N>(note: Notification) -> Result<N::Params, ExtractError<Notification>>
where
    N: lsp_types::notification::Notification,
    N::Params: serde::de::DeserializeOwned,
{
    note.extract(N::METHOD)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The caret is almost never after the closing parenthesis when signature
    /// help is wanted — it is in the middle of a call that is not yet closed.
    #[test]
    fn enclosing_call_reads_an_unfinished_call() {
        assert_eq!(
            enclosing_call("  call print_text("),
            Some(("print_text".into(), 0))
        );
        assert_eq!(
            enclosing_call("  let s: text = concat(a, "),
            Some(("concat".into(), 1))
        );
        // A finished inner call must not steal the highlight from the outer
        // one: the caret is in the outer call's second argument.
        assert_eq!(
            enclosing_call("  call print_text(concat(a, b), "),
            Some(("print_text".into(), 1))
        );
        // Nothing open: no popup, rather than a wrong one.
        assert_eq!(enclosing_call("  let x: int = 1 + 2"), None);
    }

    /// A comma inside a string literal is text, not an argument separator.
    #[test]
    fn string_literals_do_not_move_the_highlight() {
        assert_eq!(
            enclosing_call(r#"  call print_text(concat("a, b", "#),
            Some(("concat".into(), 1))
        );
        // And a caret *inside* a literal still belongs to that argument.
        assert_eq!(
            enclosing_call(r#"  call print_text(concat("a, "#),
            Some(("concat".into(), 0))
        );
    }

    /// The two halves of an `on` line, as they are typed.
    #[test]
    fn on_line_reads_the_event_and_then_the_handler() {
        assert_eq!(on_line("  on "), Some(("".into(), None)));
        assert_eq!(on_line("  on cl"), Some(("cl".into(), None)));
        assert_eq!(on_line("  on click: "), Some(("click".into(), Some("".into()))));
        assert_eq!(on_line("  on click:ok_"), Some(("click".into(), Some("ok_".into()))));
        // Not an `on` line: a variable that happens to start with the word,
        // or a statement.
        assert_eq!(on_line("  online = 1"), None);
        assert_eq!(on_line("  call print_text("), None);
    }

    #[test]
    fn parameter_labels_split_a_rendered_signature() {
        assert_eq!(
            parameter_labels("concat(text, text) -> text"),
            vec!["text", "text"]
        );
        assert_eq!(
            parameter_labels("twice(n: int): int"),
            vec!["n: int"]
        );
        assert!(parameter_labels("quit()").is_empty());
    }
}
