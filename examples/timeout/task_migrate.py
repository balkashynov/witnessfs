"""Deterministic task fixture, not an LLM session."""
import json
from pathlib import Path

config = json.loads(Path("config.json").read_text())
assert config["timeout_unit"] == "seconds"
config["timeout_unit"] = "milliseconds"
config["default_timeout"] *= 1000
Path("config.json").write_text(json.dumps(config) + "\n")
print("Migrated configuration to milliseconds; preserved the 30 second default.")
