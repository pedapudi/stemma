"""The console's chat backend: a thin frontend over the stemma ADK agent.

The heavy lifting lives elsewhere by design — the stemmadb MCP server
(integrations/mcp) owns the tools, the example agent (agents/stemma_agent)
owns model + instruction, and this module just runs the agent, mirrors the
transcript into the store's chat_log, and rebuilds resolve trajectories for
the console's inline visualization.

History note: the .stemmadb store carries operational history (query_log is
written by the resolution server; chat_log by this console). Writing chat_log
is the console's one sanctioned store write — operational memory it owns.
"""

from __future__ import annotations

import json
import os
import sqlite3
import sys
from typing import Any

_REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(_REPO, "agents"))

from google.adk.runners import Runner  # noqa: E402
from google.adk.sessions import InMemorySessionService  # noqa: E402
from google.genai import types  # noqa: E402

from stemma_agent.agent import build_agent  # noqa: E402


class AgentChat:
    def __init__(self, dbs: dict[str, str], grpc: str, endpoint: str, model: str):
        self.dbs = dbs
        self.model_name = model
        self.agent = build_agent(dbs, grpc=grpc, lm_endpoint=endpoint, lm_model=model)
        self.sessions = InMemorySessionService()
        self.runner = Runner(
            agent=self.agent, app_name="stemma-console", session_service=self.sessions
        )
        self._made: set[str] = set()

    def _store_path(self, db: str) -> str:
        return os.path.splitext(self.dbs[db])[0] + ".stemmadb"

    def _log(self, db: str, conv: str, role: str, content: str,
             trail: list[dict[str, Any]]) -> None:
        conn = sqlite3.connect(self._store_path(db))
        try:
            conn.execute(
                "INSERT INTO chat_log (conversation, role, content, trail) VALUES (?, ?, ?, ?)",
                (conv, role, content, json.dumps(trail)),
            )
            conn.commit()
        finally:
            conn.close()

    def transcript(self, db: str, conv: str = "default") -> list[dict[str, Any]]:
        conn = sqlite3.connect(f"file:{self._store_path(db)}?mode=ro", uri=True)
        try:
            rows = conn.execute(
                "SELECT role, content, trail FROM chat_log "
                "WHERE conversation = ? ORDER BY id", (conv,)
            ).fetchall()
        except sqlite3.OperationalError:
            return []
        finally:
            conn.close()
        return [
            {"role": r, "content": c, "trail": json.loads(t or "[]")}
            for r, c, t in rows
        ]

    def conversations(self, db: str) -> list[dict[str, Any]]:
        """Every conversation with its opening line — the resume list."""
        conn = sqlite3.connect(f"file:{self._store_path(db)}?mode=ro", uri=True)
        try:
            rows = conn.execute(
                "SELECT conversation, min(said_at), count(*), "
                " (SELECT content FROM chat_log c2 WHERE c2.conversation = c1.conversation "
                "  AND c2.role = 'user' ORDER BY c2.id LIMIT 1) "
                "FROM chat_log c1 GROUP BY conversation ORDER BY min(id) DESC"
            ).fetchall()
        except sqlite3.OperationalError:
            return []
        finally:
            conn.close()
        return [
            {"id": cid, "started": at, "turns": n, "title": (first or cid)[:80]}
            for cid, at, n, first in rows
        ]

    async def send(self, db: str, text: str, explain_fn,
                   conv: str = "default") -> dict[str, Any]:
        """One user turn through the agent; returns {message, trail}."""
        key = f"{db}:{conv}"
        if key not in self._made:
            await self.sessions.create_session(
                app_name="stemma-console", user_id="console", session_id=key
            )
            self._made.add(key)

        self._log(db, conv, "user", text, [])
        trail: list[dict[str, Any]] = []
        final = ""
        async for ev in self.runner.run_async(
            user_id="console",
            session_id=key,
            new_message=types.Content(role="user", parts=[types.Part(text=text)]),
        ):
            for call in ev.get_function_calls():
                args = dict(call.args or {})
                entry: dict[str, Any] = {"tool": call.name, "args": args, "result": None}
                # the console re-renders resolve as a trajectory; resolution is
                # deterministic, so re-explaining is exact and costs ~ms
                if call.name == "resolve" and args.get("query"):
                    try:
                        entry["trace"] = explain_fn(args["query"], args.get("database", db))
                    except Exception:
                        pass
                trail.append(entry)
            for resp in ev.get_function_responses():
                payload: Any = resp.response
                if isinstance(payload, dict) and "result" in payload:
                    payload = payload["result"]
                for entry in reversed(trail):
                    if entry["tool"] == resp.name and entry["result"] is None:
                        entry["result"] = _strip_trajectory(payload)
                        break
            if ev.content and ev.content.parts:
                texts = [p.text for p in ev.content.parts if getattr(p, "text", None)]
                if texts and ev.is_final_response():
                    final = "\n".join(texts)

        self._log(db, conv, "assistant", final, trail)
        return {"message": final, "trail": trail}


def _strip_trajectory(payload: Any) -> Any:
    """The full trajectory rides separately (entry['trace']); keep the tool
    result shown in the trail readable."""
    try:
        if isinstance(payload, dict):
            return {k: v for k, v in payload.items() if k != "trajectory"}
        # MCP text content sometimes arrives as a JSON string
        if isinstance(payload, str):
            d = json.loads(payload)
            if isinstance(d, dict):
                return {k: v for k, v in d.items() if k != "trajectory"}
    except (json.JSONDecodeError, TypeError):
        pass
    return payload
