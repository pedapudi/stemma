# stemmadb MCP server

stemma over the Model Context Protocol: `resolve` (with the full trajectory as
structured content), `sql` (read-only), `schema`, and `knowledge_graph` as
tools for any MCP client — agent frameworks, IDEs, chat apps.

```sh
pip install -e clients/python 'mcp<2'
STEMMADB_DBS=legal=/path/legal.db STEMMADB_GRPC=127.0.0.1:50051 \
  python integrations/mcp/stemmadb_mcp.py     # stdio transport
```

The resolve tool's contract: a compact digest sized for model context, plus
`trajectory` — every span considered, per-channel scores, snippets, and
rejected near-misses — so clients can render resolution the way the stemma
console does. See agents/stemma_agent for the reference consumer.
