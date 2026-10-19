"""SAM 2.1 video predictor over `frames/<n>.jpg`, from one box per object on the frame it shows best; `masks/<n>.png`, the union.

argv: frames dir, masks dir, json `[{"frame": n, "box": [x0, y0, x1, y1], "what": "..."}]` in pixels.
Each object is tracked alone, forward and back from its own frame: SAM 2 batches objects from the earliest prompted frame,
so one prompted later would be guessed at with no memory before its prompt.
"""
import json
import os
import pathlib
import sys

import cv2
import numpy as np
import torch
from sam2.build_sam import build_sam2_video_predictor

FILLS = 0.15  # of its box, the least an object's mask on its own frame covers
INSIDE = 0.75  # of that mask, the least inside its box grown by GROW
GROW = 0.1


def main(frames, masks, objects):
    frames, masks = pathlib.Path(frames), pathlib.Path(masks)
    n = len(list(frames.glob("*.jpg")))
    h, w = cv2.imread(str(frames / "00000.jpg")).shape[:2]
    predictor = build_sam2_video_predictor(os.environ["SAM2_CONFIG"], os.environ["SAM2_CHECKPOINT"], device="cuda")
    union = np.zeros((n, h, w), bool)
    with torch.inference_mode(), torch.autocast("cuda", dtype=torch.bfloat16):
        state = predictor.init_state(str(frames), offload_video_to_cpu=True)
        for o in objects:
            predictor.reset_state(state)
            seed, box = o["frame"], np.float32(o["box"])
            predictor.add_new_points_or_box(state, frame_idx=seed, obj_id=0, box=box)
            got = {}
            for reverse in (False, True):
                for t, _, logits in predictor.propagate_in_video(state, start_frame_idx=seed, reverse=reverse):
                    got[t] = (logits[0, 0] > 0).cpu().numpy()
            assert len(got) == n, f"{o['what']}: tracked {len(got)} of {n} frames"
            x0, y0, x1, y1 = box
            gx, gy = GROW * (x1 - x0), GROW * (y1 - y0)
            m = got[seed]
            ys, xs = np.nonzero(m)
            inside = ((xs >= x0 - gx) & (xs <= x1 + gx) & (ys >= y0 - gy) & (ys <= y1 + gy)).sum()
            fills = m.sum() / max((x1 - x0) * (y1 - y0), 1)
            if fills < FILLS or inside < INSIDE * m.sum():
                raise SystemExit(f"{o['what']}: on frame {seed} its mask covers {fills:.0%} of its box {o['box']}, "
                                 f"{inside / max(m.sum(), 1):.0%} of it inside; the box does not seed it")
            for t, mt in got.items():
                union[t] |= mt
            shown = [t for t in range(n) if got[t].any()]
            print(f"{o['what']}: seeded on frame {seed}, {fills:.0%} of its box; in frames "
                  f"{shown[0] if shown else '-'}..{shown[-1] if shown else '-'} ({len(shown)} of {n})", flush=True)
    masks.mkdir(exist_ok=True)
    for t in range(n):
        cv2.imwrite(str(masks / f"{t:05d}.png"), union[t].astype(np.uint8) * 255)


main(sys.argv[1], sys.argv[2], json.loads(sys.argv[3]))
