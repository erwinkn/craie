"""Summarizes a Pulse measurement log (React on Craie or GPUI).

    python3 bench/summarize.py <log> <label>

Reads the per-half-second lines both apps print with PULSE_LOG=1, skips
the first seven seconds, and prints ranges and medians: frames per
second, the frame cost each app measures, process CPU (the whole app:
for Craie the native threads and the JS worker; minus the headless
loop's spinning when `CRAIE_HEADLESS_LOG` lines are present), and the
work done per second, which must match between the apps.
"""

import re
import statistics as st
import sys

LINE = re.compile(
    r"\] (\d+) ms: (\d+) fps, (?:cpu|frame) ([\d.]+) ms \(max ([\d.]+)\)"
    r".*?(?:prepare ([\d.]+) ms.*?)?(?:process )?cpu (\d+)%"
    r".*?heat ([\d.]+), entries ([\d.]+), sparks ([\d.]+), ripples ([\d.]+)"
)
SPIN = re.compile(r"spun (\d+) ms of (\d+) ms")


def main(path, label):
    rows, spins = [], []
    for line in open(path):
        m = LINE.search(line)
        if m and int(m.group(1)) > 7000:
            rows.append(m.groups())
        s = SPIN.search(line)
        if s:
            spins.append(int(s.group(1)) / int(s.group(2)) * 100)
    if not rows:
        print(f"{label}: no reports after 7 s (was the window in front?)")
        return

    def col(i):
        return [float(r[i]) for r in rows if r[i] is not None]

    def span(i, fmt):
        v = col(i)
        return f"{fmt.format(min(v))}-{fmt.format(max(v))}" if v else "-"

    spin = st.median(spins[3:]) if len(spins) > 3 else 0.0
    cpu = st.median(col(5))
    prepare = f", layout+scene {span(4, '{:.2f}')} ms" if col(4) else ""
    print(
        f"{label}: {span(1, '{:.0f}')} fps, frame {span(2, '{:.2f}')} ms "
        f"(worst {max(col(3)):.1f}){prepare}, process CPU {cpu - spin:.0f}%"
        f"{f' (after {spin:.0f}% spin)' if spin else ''}; per s: heat "
        f"{st.median(col(6)):.1f}, entries {st.median(col(7)):.1f}, sparks "
        f"{st.median(col(8)):.1f}, ripples {st.mean(col(9)):.2f} ({len(rows)} reports)"
    )


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
