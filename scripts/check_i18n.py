# /// script
# requires-python = ">=3.12"
# ///
"""Checks the app's text files, ui/i18n/<code>.json, against English: every language has every
key and no others, each text has the same {placeholders} as the English one, and plural forms are
objects with at least "other". Exit code 1 with the problems listed, else "i18n OK".

    uv run scripts\\check_i18n.py
"""

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FOLDER = ROOT / "ui" / "i18n"
PLACEHOLDER = re.compile(r"\{(\w+)\}")
FORMS = {"zero", "one", "two", "few", "many", "other"}
# Texts that stay as their owners write them in every language.
PRODUCT_NAMES = {"Steam Controller", "Xbox 360 controller", "DualSense", "DualShock 4", "Switch Pro", "Steam Input", "Big Picture", "ViGEmBus", "Microsoft Store"}


def flatten(node, prefix=""):
    """Dotted keys to their values; a plural object (only form names as keys) counts as one value."""
    out = {}
    for key, value in node.items():
        path = f"{prefix}{key}"
        if isinstance(value, dict) and value and set(value) <= FORMS:
            out[path] = value
        elif isinstance(value, dict):
            out.update(flatten(value, f"{path}."))
        else:
            out[path] = value
    return out


def placeholders(value):
    texts = value.values() if isinstance(value, dict) else [value]
    return {p for text in texts if isinstance(text, str) for p in PLACEHOLDER.findall(text)} - {"count"}


def main():
    problems = []
    files = {p.stem: json.loads(p.read_text(encoding="utf-8")) for p in sorted(FOLDER.glob("*.json"))}
    if "en" not in files:
        print("ui/i18n/en.json is missing")
        return 1
    english = flatten(files["en"])
    for key, value in english.items():
        if isinstance(value, dict) and "other" not in value:
            problems.append(f"en: {key} has plural forms without \"other\"")
        if not isinstance(value, (str, dict)):
            problems.append(f"en: {key} isn't a text")
    for code, data in files.items():
        if code == "en":
            continue
        other = flatten(data)
        for key in english.keys() - other.keys():
            problems.append(f"{code}: missing {key}")
        for key in other.keys() - english.keys():
            problems.append(f"{code}: extra {key}")
        for key in english.keys() & other.keys():
            if isinstance(english[key], dict) != isinstance(other[key], dict):
                problems.append(f"{code}: {key} should {'' if isinstance(english[key], dict) else 'not '}have plural forms")
            elif isinstance(other[key], dict) and "other" not in other[key]:
                problems.append(f"{code}: {key} has plural forms without \"other\"")
            if placeholders(english[key]) != placeholders(other[key]):
                problems.append(f"{code}: {key} has placeholders {sorted(placeholders(other[key]))}, English has {sorted(placeholders(english[key]))}")
            # A text that is only placeholders and punctuation ("{game} · {store}") is the same in every
            # language, and so is a product name.
            words = re.sub(r"\{\w+\}|[\W\d_]+", "", english[key]) if isinstance(english[key], str) else ""
            if other[key] == english[key] and len(words) > 12 and not english[key].startswith("steambro") and english[key] not in PRODUCT_NAMES:
                problems.append(f"{code}: {key} is still English")
    for problem in problems:
        print(problem)
    print(f"{len(problems)} problem(s)" if problems else f"i18n OK: {len(english)} keys, {', '.join(sorted(files))}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
