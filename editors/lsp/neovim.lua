-- Kiln for Neovim (0.10+), with no plugin beyond the built-in LSP client.
-- Put this in your init.lua.
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
