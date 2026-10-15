"""Deterministic task fixture, not an LLM session."""
import json
from pathlib import Path

config = json.loads(Path("config.json").read_text())
timeout = 60000 if config["timeout_unit"] == "milliseconds" else 60
Path("retry.json").write_text(json.dumps({"timeout": timeout}) + "\n")
print(f"Read unit={config['timeout_unit']}; wrote retry timeout={timeout}.")
