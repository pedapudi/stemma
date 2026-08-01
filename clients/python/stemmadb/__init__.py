"""stemmadb: client library for the stemma resolution engine.

Two entry points:
  - StemmaClient — gRPC client for a running stemma-server (resolve/explain)
  - StoreBrowser — read-only navigation of the user DB and .stemmadb store
"""

from stemmadb.browser import StoreBrowser
from stemmadb.client import StemmaClient

__all__ = ["StemmaClient", "StoreBrowser"]
