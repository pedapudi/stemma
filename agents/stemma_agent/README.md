# stemma agent — the reference implementation

How to build an agent on stemmadb, in three moves:

1. **stemmadb speaks MCP** (`integrations/mcp/stemmadb_mcp.py`) — resolve,
   sql, schema, knowledge_graph as tools.
2. **The agent is plain ADK** — an `LlmAgent` whose toolset is that MCP server
   over stdio; any OpenAI-compatible model through LiteLLM.
3. **The instruction enforces the contract** — resolve before reference, cite
   `table.column #rowid`, read-only SQL, honest ambiguity.

```sh
pip install google-adk litellm 'mcp<2' -e clients/python
export STEMMADB_DBS=legal=/path/legal.db
export STEMMA_LM_ENDPOINT=http://host:8080/v1 STEMMA_LM_MODEL=<name>
adk run agents/stemma_agent
```

The stemma console's chat rail is a frontend over exactly this agent
(`ui/agent_backend.py`): transcripts persist in the store's `chat_log`, and
every resolve renders as an inline trajectory.
