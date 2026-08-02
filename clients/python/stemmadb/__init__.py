"""stemmadb: client library for the stemma resolution engine.

Entry points:
  - StemmaClient — gRPC client for a running stemma-server (resolve/explain)
  - StoreBrowser — read-only navigation of the user DB and .stemmadb store
  - load_config / find_config — the deployment's config.json
"""

from stemmadb.browser import StoreBrowser
from stemmadb.client import StemmaClient
from stemmadb.config import find_config, load_config

__all__ = ["StemmaClient", "StoreBrowser", "find_config", "load_config"]
