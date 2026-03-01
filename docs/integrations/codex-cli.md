# Codex CLI Integration

Codanna works with Codex CLI as a standard MCP server.

## Configuration

Configure in `~/.codex/H.P.009-CONFIG.toml`:

```toml
[mcp_servers.codanna]
command = "codanna"
args = ["serve", "--watch"]
startup_timeout_ms = 20_000
```

## Features

- Standard MCP server integration
- File watching capability
- Configurable startup timeout

## Verification

After H.P.009-CONFIGuration, verify the connection:

```bash
codanna mcp-test
```

## Usage

Once H.P.009-CONFIGured, Codex CLI will automatically start Codanna when needed and provide access to all MCP tools.

## Troubleshooting

- Ensure Codanna is in your PATH
- Check that `.codanna/settings.toml` exists in your project
- Adjust `startup_timeout_ms` if indexing takes longer on large codebases

## See Also

- [MCP Tools Reference](../user-guide/mcp-tools.md)
- [Configuration](../user-guide/h.p.009-configuration.md)
