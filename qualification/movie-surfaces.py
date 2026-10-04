#!/usr/bin/env python3
"""Run twenty independent native players concurrently on a GTK display.

Supply the compiled Rust test binary, private delivery JSON and output directory.
Private input uses [{"alias":"movie-01","payload":{NativeOpenRequest fields}}].
Use an isolated 2048x1080 X11 display; windows tile without covering each other.
"""
import argparse
import concurrent.futures
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("engine", choices=["mpv", "gstreamer"])
parser.add_argument("binary", type=Path)
parser.add_argument("cases", type=Path)
parser.add_argument("output", type=Path)
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=True)


def run(index):
    env = {
        **os.environ,
        "VIPTV_NATIVE_PROVIDER_CASES": str(args.cases.resolve()),
        "VIPTV_NATIVE_CASE_INDEX": str(index),
        "VIPTV_NATIVE_ENGINE": args.engine,
    }
    with (args.output / f"{args.engine}-{index + 1:02d}.log").open("w") as log:
        try:
            result = subprocess.run(
                [str(args.binary.resolve()), "real_movie_surface_case", "--ignored", "--nocapture", "--test-threads=1"],
                env=env, stdout=log, stderr=subprocess.STDOUT, timeout=100,
            )
            passed = result.returncode == 0
        except subprocess.TimeoutExpired:
            log.write("Native surface test exceeded 100 seconds.\n")
            passed = False
    print(f"{args.engine} movie-{index + 1:02d}: {'PASS' if passed else 'FAIL'}", flush=True)
    return passed


with concurrent.futures.ThreadPoolExecutor(max_workers=20) as pool:
    results = list(pool.map(run, range(20)))
print(f"{args.engine}: {sum(results)}/20 native surfaces passed", flush=True)
raise SystemExit(0 if all(results) else 1)
