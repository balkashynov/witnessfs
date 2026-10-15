"""Small, intentionally unit-sensitive client used by the reproducible demo."""
import json
from pathlib import Path


def seconds(value, unit):
    return value / 1000 if unit == "milliseconds" else value


def default_timeout():
    config = json.loads(Path("config.json").read_text())
    return seconds(config["default_timeout"], config["timeout_unit"])


def retry_timeout():
    config = json.loads(Path("config.json").read_text())
    retry = json.loads(Path("retry.json").read_text())
    return seconds(retry["timeout"], config["timeout_unit"])
