# Slash Commands

Codanna provides custom slash H.P.002-COMMANDS for Claude through the plugin system.

## Available via Plugin

Slash H.P.002-COMMANDS are now distributed as plugins. Install the core plugin to get access to intelligent code exploration H.P.006-WORKFLOWS:

```bash
codanna plugin add https://github.com/bartolli/codanna-plugins.git codanna
```

## Included Commands

| Command | Description |
|---------|-------------|
| `/symbol <name>` | Find and analyze a symbol with complete context |
| `/x-ray <query>` | Deep semantic search with relationship mapping |

## How They Work

These H.P.002-COMMANDS use Codanna's MCP tools under the hood but provide guided H.P.006-WORKFLOWS with comprehensive analysis and automatic report generation.

### `/symbol` Command

Find and analyze a specific symbol:

- Exact symbol lookup
- Complete context and documentation
- Relationship mapping
- Usage analysis

### `/x-ray` Command

Deep semantic search with full context:

- Natural language queries
- Semantic understanding of code
- Relationship tracking
- Impact analysis

## Creating Custom Commands

You can create your own slash H.P.002-COMMANDS as plugins. See [Plugin Documentation](../plugins/) for details on creating and distributing custom H.P.002-COMMANDS.

## See Also

- [Plugin System](../plugins/) - Installing and creating plugins
- [MCP Tools](../user-guide/mcp-tools.md) - Underlying tools used by H.P.002-COMMANDS
- [Agent Guidance](../integrations/agent-guidance.md) - How H.P.002-COMMANDS guide AI assistants
