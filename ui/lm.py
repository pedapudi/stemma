"""The conversational layer: an LLM talks to the data through stemma.

The model never free-associates over the schema: it is given three tools —
resolve (pin mentions to records), sql (read-only queries), schema — and the
system prompt requires resolution before reference. Any OpenAI-compatible
chat-completions endpoint works (vLLM, llama.cpp, LiteLLM, hosted
compatibility endpoints); configure with --lm-endpoint/--lm-model.
"""

from __future__ import annotations

import json
import os
import urllib.request
from typing import Any

SYSTEM_PROMPT = """You are the stemma console's data assistant for the database '{db}'.

Ground rules:
- Before referring to any entity, value, table or column from the data, pin it
  with the resolve tool; quote resolutions as table.column #rowid.
- Use sql (read-only SELECT) to fetch what resolve pointed at. Never invent
  table names, column names, or stored values: take them from schema/resolve.
- If resolution is ambiguous, say so and show the top candidates instead of
  guessing. If it finds nothing, say that plainly.
- Keep answers short, factual, lowercase-calm; cite rows you actually read.
"""

TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "resolve",
            "description": "Resolve natural-language mentions to database records. "
            "Returns mentions with ranked candidates (table.column #rowid, value/snippet, score).",
            "parameters": {
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "sql",
            "description": "Run a read-only SELECT against the data. Schema 'src' is the "
            "user database (e.g. src.regulations); 'main' is the stemma store.",
            "parameters": {
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "schema",
            "description": "Tables, columns and relations of the database.",
            "parameters": {"type": "object", "properties": {}},
        },
    },
]

MAX_ROUNDS = 8


class LmConfig:
    def __init__(self, endpoint: str, model: str):
        self.endpoint = endpoint.rstrip("/")
        self.model = model
        self.api_key = os.environ.get("LM_API_KEY", "")


def _post(cfg: LmConfig, payload: dict[str, Any]) -> dict[str, Any]:
    req = urllib.request.Request(
        cfg.endpoint + "/chat/completions",
        data=json.dumps(payload).encode(),
        headers={
            "content-type": "application/json",
            **({"authorization": f"Bearer {cfg.api_key}"} if cfg.api_key else {}),
        },
    )
    with urllib.request.urlopen(req, timeout=420) as r:
        return json.load(r)


def _compact_trace(trace: dict[str, Any]) -> dict[str, Any]:
    """Boil an Explain trace down to what a model needs to act on it."""
    out = []
    for i in trace.get("mentions", []):
        s = trace["spans"][i]
        out.append({
            "mention": s["text"],
            "candidates": [
                {
                    "ref": f"{c['table']}.{c['column']} #{c['rowid']}",
                    "value": c.get("snippet") or c["value"][:160],
                    "score": round(c["score"], 3),
                }
                for c in s["candidates"]
                if c["selected"]
            ],
        })
    considered = [
        s["text"] for s in trace.get("spans", [])
        if s["status"] in ("weak", "overlapped") and s["candidates"]
    ]
    return {"mentions": out, "near_misses_considered": considered[:8]}


def chat(
    cfg: LmConfig,
    db_name: str,
    messages: list[dict[str, Any]],
    resolve_fn,
    sql_fn,
    schema_fn,
) -> dict[str, Any]:
    """Runs the tool loop; returns the final message plus the tool trail."""
    convo: list[dict[str, Any]] = [
        {"role": "system", "content": SYSTEM_PROMPT.format(db=db_name)},
        *messages,
    ]
    trail: list[dict[str, Any]] = []

    for round_no in range(MAX_ROUNDS):
        # Last round: withdraw the tools so the model must answer from the
        # evidence it gathered — exploration without synthesis helps no one.
        final = round_no == MAX_ROUNDS - 1
        payload = {
            "model": cfg.model,
            # some chat templates (Qwen3.5) hard-reject mid-conversation
            # system messages, so the final-round nudge rides as user
            "messages": convo + ([{
                "role": "user",
                "content": "(tool budget exhausted — answer my question now from "
                "the evidence already gathered, citing table.column #rowid refs; "
                "no further tool calls are available)",
            }] if final else []),
            "temperature": 0.2,
            # tools stay in the request (templates need them to render the
            # history); tool_choice forbids further calls on the last round
            "tools": TOOLS,
            # thinking off for tool rounds: exploration should be fast, and
            # the evidence trail is the reasoning (harmless where unsupported)
            "chat_template_kwargs": {"enable_thinking": False},
        }
        if final:
            payload["tool_choice"] = "none"
        try:
            resp = _post(cfg, payload)
        except Exception as e:
            # a failed round must not discard the evidence already gathered
            return {"message": f"— LM endpoint error mid-conversation: {e}; "
                               "the trail shows what was gathered", "trail": trail}
        msg = resp["choices"][0]["message"]
        convo.append(msg)
        calls = msg.get("tool_calls") or []
        if not calls:
            return {"message": msg.get("content") or "", "trail": trail}

        for call in calls:
            name = call["function"]["name"]
            try:
                args = json.loads(call["function"]["arguments"] or "{}")
            except json.JSONDecodeError:
                args = {}
            try:
                if name == "resolve":
                    trace = resolve_fn(args.get("query", ""))
                    result = _compact_trace(trace)
                    trail.append({"tool": "resolve", "args": args, "result": result,
                                  "trace": trace})
                elif name == "sql":
                    result = sql_fn(args.get("query", ""))
                    result = {
                        "columns": result["columns"],
                        "rows": result["rows"][:12],
                        "truncated": result.get("truncated", False) or len(result["rows"]) > 12,
                    }
                    trail.append({"tool": "sql", "args": args, "result": result})
                elif name == "schema":
                    result = schema_fn()
                    trail.append({"tool": "schema", "args": {}, "result": result})
                else:
                    result = {"error": f"unknown tool {name}"}
            except Exception as e:  # tool errors go back to the model, verbatim
                result = {"error": str(e)}
                trail.append({"tool": name, "args": args, "result": result})
            convo.append({
                "role": "tool",
                "tool_call_id": call.get("id", name),
                "content": json.dumps({k: v for k, v in result.items() if k != "trace"})
                if isinstance(result, dict) else json.dumps(result),
            })

    return {
        "message": "— tool budget exhausted before an answer; the trail shows what was tried",
        "trail": trail,
    }
