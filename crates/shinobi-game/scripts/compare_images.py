#!/usr/bin/env python3
"""Compare two images with a tolerance. Exit 0 = match, 1 = differ, 2 = usage/size error.

  compare_images.py ACTUAL GOLDEN [--mean 1.0] [--bad-frac 0.005] [--bad-delta 24] [--diff OUT.png]
  compare_images.py --selftest

Tolerances absorb cross-GPU/driver rounding, not content changes:
  mean   : mean absolute per-channel difference (0..255) must be <= --mean
  bad    : fraction of pixels whose max channel difference exceeds --bad-delta must be <= --bad-frac
Needs Pillow (PIL).
"""
import argparse, sys
from PIL import Image, ImageChops, ImageDraw


def compare(a, b, mean_tol=1.0, bad_frac=0.005, bad_delta=24):
    a, b = a.convert("RGB"), b.convert("RGB")
    if a.size != b.size:
        return False, f"size mismatch {a.size} vs {b.size}", None
    diff = ImageChops.difference(a, b)
    raw = diff.tobytes()
    n = len(raw) // 3
    mean = sum(raw) / (3 * n)
    bad = sum(1 for i in range(0, len(raw), 3) if max(raw[i], raw[i + 1], raw[i + 2]) > bad_delta) / n
    ok = mean <= mean_tol and bad <= bad_frac
    return ok, f"mean_abs_diff={mean:.3f} (tol {mean_tol}) bad_pixels={bad*100:.3f}% (tol {bad_frac*100:.3f}%)", diff


def selftest():
    base = Image.new("RGB", (64, 36), (10, 20, 30))
    ok, msg, _ = compare(base, base.copy())
    assert ok, msg
    noisy = base.copy(); noisy.putpixel((3, 3), (12, 21, 29))            # tiny rounding noise: must pass
    assert compare(base, noisy)[0]
    changed = base.copy(); ImageDraw.Draw(changed).rectangle((10, 5, 40, 25), fill=(255, 0, 255))  # real change
    ok, msg, _ = compare(base, changed)
    assert not ok, "comparator failed to flag a visible change: " + msg
    assert not compare(base, Image.new("RGB", (32, 18)))[0]
    print("selftest ok")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("actual", nargs="?"); ap.add_argument("golden", nargs="?")
    ap.add_argument("--mean", type=float, default=1.0)
    ap.add_argument("--bad-frac", type=float, default=0.005)
    ap.add_argument("--bad-delta", type=int, default=24)
    ap.add_argument("--diff"); ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    if a.selftest:
        return selftest()
    if not (a.actual and a.golden):
        ap.error("need ACTUAL and GOLDEN")
    ok, msg, diff = compare(Image.open(a.actual), Image.open(a.golden), a.mean, a.bad_frac, a.bad_delta)
    print(("MATCH " if ok else "DIFFER ") + msg)
    if diff is not None and a.diff and not ok:
        diff.point(lambda v: min(255, v * 4)).save(a.diff); print("diff image:", a.diff)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
