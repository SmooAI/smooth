"""Ensure taskapi.py (written by the agent) is importable when pytest
runs from the workspace root. Avoids "ModuleNotFoundError: taskapi"
when the user has no editable install and is just running pytest
directly in the workspace."""

import sys
from pathlib import Path

WORKSPACE = Path(__file__).parent.resolve()
if str(WORKSPACE) not in sys.path:
    sys.path.insert(0, str(WORKSPACE))
