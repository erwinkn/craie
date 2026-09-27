#!/usr/bin/env python3
"""E19 report: joins the probe's stamps (<base>.csv) with the app's
(<base>-js.csv, <base>-gc.csv) and prints the round trip and its phases
as p50/p95/p99/max, in ms. An unanswered click counts as an infinite
round trip. The frame ends when the GPU finished (headless) or when
`present` returned (windowed; the display shows it a vsync or two
later).

  python3 bench/e19/report.py /tmp/craie-e19/idle.csv "idle" headless|windowed
"""
import csv
import math
import sys


def num(v):
    try:
        return int(v)
    except ValueError:
        try:
            return float(v)
        except ValueError:
            return v


def rows(path):
    try:
        with open(path) as f:
            return [{k: num(v) for k, v in r.items()} for r in csv.DictReader(f)]
    except FileNotFoundError:
        return []


def pct(xs, p):
    """Nearest rank."""
    xs = sorted(xs)
    return xs[max(0, math.ceil(p / 100 * len(xs)) - 1)]


def main():
    path = sys.argv[1]
    label = sys.argv[2] if len(sys.argv) > 2 else path
    drawn = "presented" if sys.argv[3:] == ["windowed"] else "GPU done"
    base = path[:-4] if path.endswith(".csv") else path
    native = rows(path)
    js = {r["seq"]: r for r in rows(base + "-js.csv")}
    gc = rows(base + "-gc.csv")
    frames = rows(base + "-frames.csv")
    done = [r for r in native if r["presented"]]
    lost = len(native) - len(done)
    for r in native:
        r.setdefault("due", r["dispatched"])
    joined = [(r, js[r["seq"]]) for r in done if r["seq"] in js and js[r["seq"]]["committed"]]
    phases = [
        ("wait", "click → native free", lambda r, j: r["dispatched"] - r["due"]),
        ("js", "native → React committed", lambda r, j: j["committed"] - r["dispatched"]),
        ("deliver", "native → handler", lambda r, j: j["handler"] - r["dispatched"]),
        ("react", "handler → committed", lambda r, j: j["committed"] - j["handler"]),
        ("apply", "committed → applied", lambda r, j: r["applied"] - j["committed"]),
        ("paint", f"applied → {drawn}", lambda r, j: r["presented"] - r["applied"]),
    ]
    print(f"E19 {label}: {len(native)} clicks, {lost} unanswered")
    print(f"  {'ms':<34}{'p50':>8}{'p95':>8}{'p99':>8}{'max':>8}")
    trip = [(r["presented"] - r["due"]) / 1e6 for r in done] + [math.inf] * lost
    rows_ = [("round trip", f"click → {drawn}", trip)]
    rows_ += [(n, w, [f(r, j) / 1e6 for r, j in joined]) for n, w, f in phases]
    for name, what, xs in rows_:
        if xs:
            print(f"  {name + ' (' + what + ')':<34}" + "".join(f"{pct(xs, p):>8.2f}" for p in (50, 95, 99, 100)))
    if gc:
        kinds = {}
        for g in gc:
            kinds.setdefault(g["kind"], []).append(g["duration"] / 1e6)
        summary = ", ".join(f"{len(v)} {k} (max {max(v):.1f})" for k, v in sorted(kinds.items()))
        long = [(g["start"], g["start"] + g["duration"]) for g in gc if g["duration"] >= 2e6]
        hit = [r for r, j in joined if any(s < j["committed"] and e > r["dispatched"] for s, e in long)]
        print(f"  gc: {summary}; {len(hit)} clicks were in JS during a pause of 2 ms or more")
    if frames:
        fps = pct([f["fps"] for f in frames], 50)
        cpu = pct([f["cpuMs"] for f in frames], 50)
        worst = max(f["maxCpuMs"] for f in frames)
        print(f"  native frames: {fps:.0f} fps, {cpu:.1f} ms CPU each (median of per-second means; worst {worst:.1f})")


main()
