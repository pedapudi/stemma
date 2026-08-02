"""The stemma agent — the reference implementation for building agents on stemmadb.

The pattern, in three moves:

1. stemmadb speaks MCP (integrations/mcp/stemmadb_mcp.py): resolve, sql,
   schema, knowledge_graph as tools, with resolve returning the full
   trajectory as structured content.
2. The agent is plain ADK: an LlmAgent whose toolset is that MCP server over
   stdio. Any OpenAI-compatible model works through LiteLLM (vLLM, llama.cpp,
   LiteLLM proxies, hosted endpoints).
3. The system instruction enforces stemma's contract: resolve before
   reference, cite table.column #rowid, read-only SQL.

Anything that speaks MCP — other agent frameworks, IDEs, chat apps — gets the
same tools the same way; this file is just the smallest complete example.

Configuration is explicit: build_agent() takes everything as arguments, and
the `adk run` entrypoint reads the repository's config.json (databases +
console.lm). Never environment variables.

Standalone (ADK web/cli):
    adk run agents/stemma_agent      # uses the repo's config.json
"""

from __future__ import annotations

import os
import sys

from google.adk.agents import LlmAgent
from google.adk.models.lite_llm import LiteLlm
from google.adk.tools.mcp_tool import StdioConnectionParams
from google.adk.tools.mcp_tool.mcp_toolset import McpToolset
from mcp import StdioServerParameters

_HERE = os.path.dirname(os.path.abspath(__file__))
_REPO = os.path.dirname(os.path.dirname(_HERE))
_MCP_SERVER = os.path.join(_REPO, "integrations", "mcp", "stemmadb_mcp.py")
sys.path.insert(0, os.path.join(_REPO, "clients", "python"))

INSTRUCTION = """You are the stemma data assistant.

Ground rules:
- ALWAYS resolve first. For every user question your first action is the
  resolve tool with the question (or its key phrases). Never conclude the
  data lacks something from the question's wording alone — resolution is
  semantic and finds records that share no words with the question. You may
  claim absence only after resolve returned nothing useful, and then say
  what the nearest near-misses were.
- Before referring to any entity, value, table or column from the data, pin it
  with the resolve tool; cite resolutions as table.column #rowid.
- Use sql (read-only SELECT) to fetch the rows resolve pointed at — by their
  rowids (WHERE id IN (...)), not by LIKE scans. LIKE over document text is a
  last resort for when resolve found nothing. Never invent table names,
  column names, or stored values — take them from schema/resolve.
- Answer from the fetched rows or not at all. If the data does not answer
  the question, say exactly that and list the nearest near-misses from
  resolve — never fill the gap with general knowledge.
- knowledge_graph orients you in an unfamiliar corpus: characteristic terms,
  named entities, join paths. Consult it before deciding what a database
  does or does not contain.
- If resolution is ambiguous, say so and show the top candidates instead of
  guessing. If it finds nothing, say that plainly.
- Keep answers short, factual, lowercase-calm; cite rows you actually read.
- Answer from gathered evidence promptly; do not explore beyond what the
  question needs."""


def build_agent(
    dbs: dict[str, str],
    grpc: str = "127.0.0.1:50051",
    lm_endpoint: str | None = None,
    lm_model: str | None = None,
    api_key: str = "",
) -> LlmAgent:
    """An LlmAgent wired to the stemmadb MCP server for the given databases."""
    if not (lm_endpoint and lm_model):
        raise ValueError("an OpenAI-compatible endpoint and model are required")

    mcp_args = [os.path.abspath(_MCP_SERVER), "--grpc", grpc]
    for name, path in sorted(dbs.items()):
        mcp_args += ["--db", f"{name}={path}"]
    toolset = McpToolset(
        connection_params=StdioConnectionParams(
            server_params=StdioServerParameters(
                command=sys.executable,
                args=mcp_args,
            ),
            timeout=30,
        )
    )

    model = LiteLlm(
        model=f"openai/{lm_model}",
        api_base=lm_endpoint,
        api_key=api_key or "x",  # LiteLLM insists on a token; local servers ignore it
        temperature=0.2,
        # reasoning off for tool rounds: the evidence trail is the reasoning
        # (silently ignored by endpoints without thinking modes)
        extra_body={"chat_template_kwargs": {"enable_thinking": False}},
    )

    return LlmAgent(
        name="stemma_assistant",
        model=model,
        instruction=INSTRUCTION
        + f"\n\nAvailable databases: {', '.join(sorted(dbs))}. "
        "Every tool takes the database name as its `database` argument.",
        tools=[toolset],
    )


def _agent_from_config() -> LlmAgent | None:
    """`adk run` / `adk web` entrypoint: built from the repo's config.json."""
    from stemmadb import find_config, load_config

    path = find_config(_REPO)
    if path is None:
        return None
    cfg = load_config(path)
    console = cfg.get("console") or {}
    lm = console.get("lm") or {}
    if not (cfg.get("databases") and lm.get("endpoint") and lm.get("model")):
        return None
    return build_agent(
        cfg["databases"],
        grpc=console.get("grpc", "127.0.0.1:50051"),
        lm_endpoint=lm["endpoint"],
        lm_model=lm["model"],
        api_key=lm.get("api_key", ""),
    )


root_agent = _agent_from_config()
