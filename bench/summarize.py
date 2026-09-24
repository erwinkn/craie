"""Summarizes a Pulse measurement log (React on Craie or GPUI).

    python3 bench/summarize.py <log> <label>

Reads the per-half-second lines both apps print with PULSE_LOG=1 and
skips the first seven seconds. Prints ranges of frames per second and of
the frame cost each app measures, the work done per second (which must
match between the apps), and process CPU over the steady interval: the
difference of the cumulative process CPU totals between the first and
the last steady report, over the wall time between them (both from the
lines' epoch stamps). For Craie that is the whole process (native
threads and the JS worker). A headless Craie run spins between frames;
with `CRAIE_HEADLESS_LOG` its spin CPU (thread CPU clock, cumulative,
epoch-stamped) is interpolated at the same two instants and taken out.
"""

import re
import statistics as st
import sys

LINE = re.compile(
    r"\] (\d+) ms: (\d+) fps, (?:cpu|frame) ([\d.]+) ms \(max ([\d.]+)\)"
    r".*?(?:prepare ([\d.]+) ms.*?)?cpu (\d+)% \(total ([\d.]+) ms at epoch (\d+)\)"
    r".*?heat ([\d.]+), entries ([\d.]+), sparks ([\d.]+), ripples ([\d.]+)"
)
SPIN = re.compile(r"epoch (\d+): spin cpu ([\d.]+) ms in total")


def interpolate(points, t):
    """The cumulative value at epoch `t` from (epoch, total) points."""
    if not points:
        return 0.0
    if t <= points[0][0]:
        return points[0][1] * t / points[0][0] if points[0][0] else 0.0
    for (t0, v0), (t1, v1) in zip(points, points[1:]):
        if t0 <= t <= t1:
            return v0 + (v1 - v0) * (t - t0) / (t1 - t0)
    return points[-1][1]


def main(path, label):
    rows, spins = [], []
    for line in open(path):
        m = LINE.search(line)
        if m and int(m.group(1)) > 7000:
            rows.append(m.groups())
        s = SPIN.search(line)
        if s:
            spins.append((int(s.group(1)), float(s.group(2))))
    if len(rows) < 2:
        print(f"{label}: too few reports after 7 s (was the window in front?)")
        return

    def col(i):
        return [float(r[i]) for r in rows if r[i] is not None]

    def span(i, fmt):
        v = col(i)
        return f"{fmt.format(min(v))}-{fmt.format(max(v))}" if v else "-"

    (cpu0, t0), (cpu1, t1) = (float(rows[0][6]), int(rows[0][7])), (float(rows[-1][6]), int(rows[-1][7]))
    wall = t1 - t0
    spin = interpolate(spins, t1) - interpolate(spins, t0) if spins else 0.0
    cpu = (cpu1 - cpu0 - spin) / wall * 100
    prepare = f", layout+scene {span(4, '{:.2f}')} ms" if col(4) else ""
    spun = f" (after {spin / wall * 100:.0f}% spin)" if spins else ""
    print(
        f"{label}: {span(1, '{:.0f}')} fps, frame {span(2, '{:.2f}')} ms "
        f"(worst {max(col(3)):.1f}){prepare}, process CPU {cpu:.0f}%{spun} over "
        f"{wall / 1000:.1f} s; per s: heat {st.median(col(8)):.1f}, entries "
        f"{st.median(col(9)):.1f}, sparks {st.median(col(10)):.1f}, ripples "
        f"{st.mean(col(11)):.2f} ({len(rows)} reports)"
    )


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
