"""Tiny helper for Python Kurnace modules (spec §9.2 protocol).

Modules run with `python3 -I`, so they import this via a path relative to
their own file (see each run.py). Standard library only.
"""
import json
import sys


def request():
    return json.load(sys.stdin)


def emit(**ev):
    sys.stdout.write(json.dumps(ev) + "\n")
    sys.stdout.flush()


def progress(pct, msg=""):
    emit(type="progress", pct=pct, msg=msg)


def log(msg, level="info"):
    emit(type="log", level=level, msg=msg)


def output(name, path):
    emit(type="output", name=name, path=str(path))


def main(fn):
    try:
        fn(request())
        emit(type="done")
    except Exception as e:  # report, don't traceback onto the protocol stream
        emit(type="error", msg=f"{type(e).__name__}: {e}")
        sys.exit(1)
