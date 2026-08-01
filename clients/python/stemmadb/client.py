"""gRPC client for a running stemma-server."""

from __future__ import annotations

from typing import Any

import grpc

from stemmadb import _proto  # noqa: F401  (sys.path shim for generated stubs)
from stemma.v1 import resolve_pb2, resolve_pb2_grpc


class StemmaClient:
    """Talks to stemma-server's Resolve API.

    >>> with StemmaClient("127.0.0.1:50051") as c:
    ...     resp = c.resolve("the Q3 numbers for the Seattle office", database="mini")
    ...     trace = c.explain("what did Chen's team ship", database="mini")
    """

    def __init__(self, target: str = "127.0.0.1:50051", timeout: float = 10.0):
        self._channel = grpc.insecure_channel(target)
        self._stub = resolve_pb2_grpc.ResolveServiceStub(self._channel)
        self._timeout = timeout

    def __enter__(self) -> "StemmaClient":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    def close(self) -> None:
        self._channel.close()

    def _request(self, query: str, database: str) -> resolve_pb2.ResolveRequest:
        return resolve_pb2.ResolveRequest(query=query, database=database)

    def resolve(self, query: str, database: str) -> resolve_pb2.ResolveResponse:
        """Selected mentions with their candidates and evidence."""
        return self._stub.Resolve(self._request(query, database), timeout=self._timeout)

    def explain(self, query: str, database: str) -> resolve_pb2.ExplainResponse:
        """The full resolution trajectory, near-misses included."""
        return self._stub.Explain(self._request(query, database), timeout=self._timeout)

    def explain_dict(self, query: str, database: str) -> dict[str, Any]:
        """explain() as a plain dict (JSON-ready), preserving zero fields."""
        from google.protobuf.json_format import MessageToDict

        return MessageToDict(
            self.explain(query, database),
            preserving_proto_field_name=True,
            always_print_fields_with_no_presence=True,
        )
