// Thin client: everything intelligent lives in `kiln lsp` and
// `kiln dap`, so this file only starts them and gets out of the way.
const { workspace, debug, DebugAdapterExecutable } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

/// The toolchain binary, which is both the language server and the debug
/// adapter. One setting rather than two: they are the same program, and two
/// paths that could disagree would be two ways to get it wrong.
function toolchain() {
  return workspace.getConfiguration("kiln").get("serverPath", "kiln");
}

function activate(context) {
  const command = toolchain();

  const serverOptions = {
    run: { command, args: ["lsp"], transport: TransportKind.stdio },
    debug: { command, args: ["lsp"], transport: TransportKind.stdio },
  };

  client = new LanguageClient(
    "kiln",
    "Kiln Language Server",
    serverOptions,
    { documentSelector: [{ scheme: "file", language: "kiln" }] }
  );
  context.subscriptions.push(client.start());

  // The adapter is a subprocess on stdio, exactly like the language server.
  // Resolved at launch rather than named in package.json so that the setting
  // is read then, and a user who moves their toolchain does not have to
  // reinstall the extension.
  context.subscriptions.push(
    debug.registerDebugAdapterDescriptorFactory("kiln", {
      createDebugAdapterDescriptor() {
        return new DebugAdapterExecutable(toolchain(), ["dap"]);
      },
    })
  );

  // Pressing F5 with no launch.json debugs the file in front of you. Without
  // this VS Code asks the user to write a configuration first, which is a
  // poor answer for a language whose programs are usually one file.
  context.subscriptions.push(
    debug.registerDebugConfigurationProvider("kiln", {
      resolveDebugConfiguration(folder, config) {
        if (config.type || config.request || config.name) {
          return config;
        }
        const editor = require("vscode").window.activeTextEditor;
        if (!editor || editor.document.languageId !== "kiln") {
          return config;
        }
        return {
          type: "kiln",
          request: "launch",
          name: "Debug the current file",
          program: editor.document.fileName,
          cwd: folder ? folder.uri.fsPath : undefined,
        };
      },
    })
  );
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
