# Editors and the language server

Kiln ships a language server, `kiln lsp`, that speaks LSP over stdio. Any
LSP-capable editor becomes a Kiln editor by pointing it at that command —
the intelligence lives in one server rather than in any single editor.

## What works today

| Feature | Status |
| --- | --- |
| Diagnostics (parse + type/semantic errors, as you type) | ✅ |
| Completion (commands, components, `id.` properties and events, locals, keywords) | ✅ |
| Event wiring: `on ` offers the component's events; the handler position offers existing subroutines or creates one | ✅ |
| Signature help while typing a call | ✅ |
| Hover (command signatures, subroutine headers, property type and editor, declaration sites) | ✅ |
| Go to definition | ✅ |
| Find references (shadowing-aware) | ✅ |
| Document symbols (outline) | ✅ |
| Syntax highlighting | ✅ (VS Code grammar included) |
| Rename, formatting | planned |

Navigation is backed by a token-level index rather than the AST, so completion
and go-to-definition keep working while the file is half-typed and does not
parse — which is most of the time you are actually editing.

Diagnostics cover the full front end: unknown commands, argument-count and type
mismatches, assignment to a `let`, undefined variables, non-boolean conditions,
unknown component types, bad property names and types, and every parse error.
Each is reported at the name it is about, so a line with two calls underlines
the one that is wrong; a diagnostic that knows only the line underlines the
line.

A diagnostic names the fix where the toolchain can see one:

- a command from a library the module has not `use`d says which line to add —
  ``unknown command `file_read_text` — it is in the `file` library: add `use
  file` to the module``. The server loads every library's metadata the first
  time this is needed, so the first such diagnostic in a session is slower;
- a name one typo away from a real one says which — ``did you mean
  `print_text`?`` — for commands, subroutines, variables, properties, events
  and handlers;
- a handler with the wrong signature shows what the event hands over, what
  the handler takes, and the header to paste;
- `sys_sleep_ms` in a module that declares a form is refused, and the message
  points at `timer`: a windowed program does not wait, it declares a timer and
  does the work in its `on tick` handler.

## Wiring an event

Inside a component block, type `on ` and completion lists that component's
events, each with what it hands a handler. After the `:`, it lists the
module's subroutines — and, first, one that does not exist yet, named
`<id>_<event>`. Accepting it inserts the name and appends

```
sub countdown_tick(n: int)
  
end
```

at the end of the file, with the parameter list the event declares. That is
the whole loop: place a component, wire an event, write the body.

The server locates the Kiln runtime three ways, in order: by walking up from
the editor's workspace root looking for `runtime/kiln_core.h`; failing that,
from `KILN_RUNTIME_DIR`, which an installed toolchain sets; failing that, by
walking up from the `kiln` binary itself — which is how a relocatable bundle
finds its own runtime when your project lives somewhere else entirely, as
anything made from a template does. If none of the three finds one it stays up
and serves parse errors, reporting the degradation as a diagnostic on line 1
rather than going silent.

## Neovim

Neovim 0.10 or later needs no plugin — this is `editors/lsp/neovim.lua`:

```lua
vim.filetype.add({ extension = { kiln = "kiln" } })

vim.api.nvim_create_autocmd("FileType", {
  pattern = "kiln",
  callback = function(args)
    vim.bo[args.buf].commentstring = "// %s"
    vim.lsp.start({
      name = "kiln",
      cmd = { "kiln", "lsp" },
      root_dir = vim.fs.root(args.buf, { "project.kproj", ".git" }),
    })
  end,
})
```

## VS Code

`editors/vscode/` is a working extension: syntax highlighting, snippets
(`program`, `form`, `record`, `class`, `foreach`, `dll`, …), `.kiln` file
association, a debugger, and an LSP client that launches `kiln lsp`. It is not published
to the marketplace, so install it locally:

```sh
cd editors/vscode
npm install
# then either symlink it into your extensions folder…
ln -s "$PWD" ~/.vscode/extensions/kiln
# …or package it
npx @vscode/vsce package && code --install-extension kiln-*.vsix
```

Set `kiln.serverPath` if `kiln` is not on your `PATH`.

## Kate

Kate needs two things: a syntax definition, and a language-server entry that
keys on it.

`kiln install --editors` writes the first and prints the second. By hand:

```sh
mkdir -p ~/.local/share/org.kde.syntax-highlighting/syntax
cp editors/kate/kiln.xml ~/.local/share/org.kde.syntax-highlighting/syntax/
```

That is `editors/kate/kiln.xml`, and it does more than colour: it gives a
`.kiln` document a highlighting mode named **Kiln**, which is what Kate's LSP
client matches a server against. Without it there is no mode to key on and the
server never starts.

Then turn the LSP client on — **Settings → Configure Kate → Plugins → LSP
Client** — and put this in **Settings → Configure Kate → LSP Client → User
Server Settings**, merging it with whatever is already there rather than
replacing the file:

```json
{
    "servers": {
        "kiln": {
            "command": ["kiln", "lsp"],
            "highlightingModeRegex": "^Kiln$",
            "documentLanguageId": "kiln",
            "rootIndicationFileNames": ["project.kproj"]
        }
    }
}
```

Kate keeps its own allowlist of server command lines and will ask once, the
first time it starts `kiln lsp`. Say yes. `kiln install --editors` deliberately
does not write that answer for you — a tool that edited the allowlist would be
answering a security question on your behalf.

`kiln` has to be on your `PATH` for `command` to find it; give the full path
instead if it is not, or run `kiln install` first.

## Helix

Append `editors/lsp/helix-languages.toml` to `~/.config/helix/languages.toml`:

```toml
[language-server.kiln]
command = "kiln"
args = ["lsp"]

[[language]]
name = "kiln"
scope = "source.kiln"
file-types = ["kiln"]
roots = ["project.kproj"]
comment-token = "//"
block-comment-tokens = { start = "/*", end = "*/" }
indent = { tab-width = 4, unit = "    " }
language-servers = ["kiln"]
```

## Zed

Merge `editors/lsp/zed-settings.json` into `~/.config/zed/settings.json`:

```json
{
  "file_types": { "Kiln": ["kiln"] },
  "lsp": { "kiln": { "binary": { "path": "kiln", "arguments": ["lsp"] } } },
  "languages": { "Kiln": { "language_servers": ["kiln"], "tab_size": 4 } }
}
```

## Sublime Text

`editors/sublime/` holds a syntax definition and the client entry for the
[LSP](https://packagecontrol.io/packages/LSP) package:

1. Copy `Kiln.sublime-syntax` and `Comments.tmPreferences` into your
   `Packages/User` folder (**Preferences → Browse Packages…**).
2. Install **LSP** from Package Control.
3. Merge `LSP-kiln.sublime-settings` into **Preferences → Package Settings →
   LSP → Settings**:

```json
{
  "clients": {
    "kiln": { "enabled": true, "command": ["kiln", "lsp"], "selector": "source.kiln" }
  }
}
```

## JetBrains IDEs

IntelliJ IDEA, CLion, Rider and the rest need no Kiln plugin.

1. **Highlighting:** **Settings → Editor → TextMate Bundles → +**, and choose
   `editors/vscode`. It is a TextMate bundle as well as the VS Code extension.
2. **The server:** install **LSP4IJ** from the Marketplace, then **Settings →
   Languages & Frameworks → Language Servers → +** with command `kiln lsp` and
   file name pattern `*.kiln` (language id `kiln`). `editors/jetbrains/lsp4ij-kiln.json`
   holds the same settings.

## Putting `kiln` on your PATH

Every configuration above runs `kiln lsp`, so the shell has to be able to find
`kiln`. `kiln install` copies the tree it is run from to a prefix and links the
binaries into `<prefix>/bin`:

```sh
kiln install                 # /usr/local, or ~/.local when that is not writable
kiln install --user          # the same as --prefix ~/.local
kiln install --prefix /opt   # anywhere
kiln install --dry-run       # print what it would write, write nothing
kiln install --editors       # and Kate's syntax definition
```

The tree is relocatable — `kiln` finds its runtime by walking up from its own
executable — so `<prefix>/lib/kiln` is self-contained and `<prefix>/bin` holds
symlinks into it. To uninstall, delete that directory and those links; there is
no database to keep in step.

## Debugging the server

The server logs to stderr (stdout is the protocol channel — a stray print there
corrupts the stream). Watch it with your editor's LSP log, or drive it by hand:

```sh
kiln lsp   # then send framed JSON-RPC on stdin
```

The repository's `cli/tests/lsp.rs` does exactly that, and is the reference
for the wire format.
