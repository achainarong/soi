#!/usr/bin/env python3
"""Writes src/agents/context-windows.tsv: the context window of each model of
the major vendors, from models.dev. soi uses it when an agent reports how
much context it uses but not how much it has.

    scripts/context_windows.py               download https://models.dev/api.json
    scripts/context_windows.py api.json      use a copy, e.g. OpenCode's
                                             ~/.cache/opencode/models.json
"""

import datetime
import json
import pathlib
import sys
import urllib.request

SOURCE = "https://models.dev/api.json"
# The vendors' own APIs: resellers list the same models, sometimes with
# smaller windows.
PROVIDERS = [
    "anthropic",
    "openai",
    "google",
    "xai",
    "zai",
    "zhipuai",
    "deepseek",
    "mistral",
    "moonshotai",
    "alibaba",
    "minimax",
    "meta",
]
OUTPUT = pathlib.Path(__file__).resolve().parent.parent / "src/agents/context-windows.tsv"


def main() -> None:
    if len(sys.argv) > 1:
        catalog = json.loads(pathlib.Path(sys.argv[1]).read_text())
    else:
        with urllib.request.urlopen(SOURCE, timeout=30) as response:
            catalog = json.load(response)
    windows: dict[str, int] = {}
    for provider in PROVIDERS:
        for model_id, model in catalog.get(provider, {}).get("models", {}).items():
            context = (model.get("limit") or {}).get("context")
            if isinstance(context, int) and context > 0:
                # The first vendor listing a model is the one that makes it.
                windows.setdefault(model_id.lower(), context)
    lines = [
        f"# From {SOURCE} ({', '.join(PROVIDERS)}), {datetime.date.today()}.",
        "# Regenerate with scripts/context_windows.py.",
    ]
    lines += [f"{model_id}\t{context}" for model_id, context in sorted(windows.items())]
    OUTPUT.write_text("\n".join(lines) + "\n")
    print(f"{len(windows)} models -> {OUTPUT}")


if __name__ == "__main__":
    main()
