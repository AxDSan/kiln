# Kiln in JetBrains IDEs

IntelliJ IDEA, CLion, Rider and the rest need no Kiln plugin: they read a
TextMate grammar for highlighting and speak LSP through the LSP4IJ plugin, and
Kiln ships both.

## Highlighting

1. **Settings → Editor → TextMate Bundles → +**
2. Choose the `editors/vscode` directory of your Kiln install. It is a TextMate
   bundle as well as a VS Code extension, so the same grammar highlights both.

## Diagnostics, completion, hover, outline and formatting

1. Install **LSP4IJ** from the Marketplace (by Red Hat).
2. **Settings → Languages & Frameworks → Language Servers → +**
3. Fill in:
   - **Name:** `Kiln`
   - **Command:** `kiln lsp` (or the full path to the `kiln` binary)
   - **Mappings → File name patterns:** `*.kiln`, language id `kiln`

`lsp4ij-kiln.json` beside this file holds the same settings for LSP4IJ's
**Import** button.
