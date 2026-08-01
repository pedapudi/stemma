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

Standalone (ADK web/cli):
    export STEMMADB_DBS=legal=/path/to/legal.db
    export STEMMA_LM_ENDPOINT=http://host:8080/v1 STEMMA_LM_MODEL=qwen3.5-35b
    adk run agents/stemma_agent
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
_MCP_SERVER = os.path.join(_HERE, "..", "..", "integrations", "mcp", "stemmadb_mcp.py")

INSTRUCTION = """You are the stemma data assistant.

Ground rules:
- Before referring to any entity, value, table or column from the data, pin it
  with the resolve tool; cite resolutions as table.column #rowid.
- Use sql (read-only SELECT) to fetch what resolve pointed at. Never invent
  table names, column names, or stored values — take them from schema/resolve.
- knowledge_graph orients you in an unfamiliar corpus: characteristic terms,
  named entities, join paths.
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
    lm_endpoint = lm_endpoint or os.environ.get("STEMMA_LM_ENDPOINT", "")
    lm_model = lm_model or os.environ.get("STEMMA_LM_MODEL", "")
    if not (lm_endpoint and lm_model):
        raise ValueError("an OpenAI-compatible endpoint and model are required")

    toolset = McpToolset(
        connection_params=StdioConnectionParams(
            server_params=StdioServerParameters(
                command=sys.executable,
                args=[os.path.abspath(_MCP_SERVER)],
                env={
                    **os.environ,
                    "STEMMADB_GRPC": grpc,
                    "STEMMADB_DBS": ",".join(f"{k}={v}" for k, v in dbs.items()),
                },
            ),
            timeout=30,
        )
    )

    model = LiteLlm(
        model=f"openai/{lm_model}",
        api_base=lm_endpoint,
        api_key=api_key or os.environ.get("LM_API_KEY", "x"),
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


def _dbs_from_env() -> dict[str, str]:
    out: dict[str, str] = {}
    for part in filter(None, os.environ.get("STEMMADB_DBS", "").split(",")):
        name, _, path = part.partition("=")
        if name and path:
            out[name] = path
    return out


# `adk run` / `adk web` entrypoint (configured entirely by environment).
root_agent = None
if _dbs_from_env() and os.environ.get("STEMMA_LM_ENDPOINT"):
    root_agent = build_agent(_dbs_from_env())
