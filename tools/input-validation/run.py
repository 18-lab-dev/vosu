#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))


from validation_error import ValidationError
from capture import capture
from input_validation import create_keyboard_backend, navigate_ui, navigation_key_names, normalize_key_name, scan_maps
from mode_plans import parse_play_plan
from mode_runner import (
    create_plan_backends,
    key_mapping_for_plan,
    live_play_plan,
    parse_rectangle,
    play_plan,
)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Validate precise keypress input against local beatmaps and live vosu data.")
    subparsers = parser.add_subparsers(dest="command", required=True)

    scan = subparsers.add_parser("scan-maps", help="find mania maps by long-note ratio")
    scan.add_argument("--songs", required=True, type=Path, help="osu! Songs directory to scan recursively")
    scan.add_argument("--min-ln-ratio", required=True, type=float, help="minimum LN/normal-note ratio")
    scan.add_argument("--limit", required=True, type=int, help="maximum number of maps to print")

    live = subparsers.add_parser("capture", help="capture additive precise keypress transitions")
    live.add_argument("--base-url", required=True, help="vosu base URL, such as http://127.0.0.1:24050")
    live.add_argument("--label", required=True, help="target label stored in capture metadata")
    live.add_argument("--out", required=True, type=Path, help="capture JSON output file")
    live.add_argument("--timeout", required=True, type=float, help="capture duration and connection timeout in seconds")
    navigate = subparsers.add_parser(
        "navigate", help="navigate an osu! client from the idle logo using keyboard shortcuts"
    )
    navigate.add_argument("--client", required=True, choices=("lazer", "stable"))
    navigate.add_argument(
        "--target", required=True, choices=("song-select", "multiplayer")
    )
    navigate.add_argument(
        "--backend", choices=("auto", "windows", "uinput", "dry-run"), default="auto",
        help="keyboard injection backend (default: auto)",
    )
    navigate.add_argument("--search", help="song-select search text to enter after navigation")
    navigate.add_argument(
        "--key-delay", type=float, default=0.1,
        help="seconds between menu shortcut keys (default: 0.1)",
    )
    navigate.add_argument(
        "--typing-delay", type=float, default=0.001,
        help="seconds between song-search characters (default: 0.001)",
    )
    navigate.add_argument(
        "--transition-delay", type=float, default=1.0,
        help="seconds to wait for song select before focusing search (default: 1.0)",
    )

    autoplay_parser = subparsers.add_parser("autoplay", help="play any osu! mode from a beatmap plan")
    autoplay_parser.add_argument("map", type=Path, help="path to an osu! .osu file")
    autoplay_parser.add_argument(
        "--backend", choices=("auto", "windows", "uinput", "dry-run"), default="auto",
        help="keyboard and pointer injection backend (default: auto)",
    )
    autoplay_parser.add_argument(
        "--start-delay", type=float, default=2.0,
        help="seconds to wait before the beatmap timeline starts (default: 2.0)",
    )
    autoplay_parser.add_argument(
        "--tap-duration", type=int, default=40,
        help="normal-note press duration in milliseconds (default: 40)",
    )
    autoplay_parser.add_argument(
        "--keys", help="comma-separated controls; mode-specific defaults cover standard layouts",
    )
    autoplay_parser.add_argument(
        "--playfield", help="desktop playfield rectangle x,y,width,height for osu!/catch",
    )
    autoplay_parser.add_argument(
        "--desktop", help="Linux virtual desktop rectangle x,y,width,height for uinput",
    )

    live_autoplay_parser = subparsers.add_parser(
        "live-autoplay", help="play any osu! mode synchronized to live vosu state"
    )
    live_autoplay_parser.add_argument("map", type=Path, help="path to an osu! .osu file")
    live_autoplay_parser.add_argument(
        "--base-url", required=True, help="vosu base URL, such as http://127.0.0.1:24050"
    )
    live_autoplay_parser.add_argument(
        "--backend", choices=("auto", "windows", "uinput", "dry-run"), default="auto",
        help="keyboard and pointer injection backend (default: auto)",
    )
    live_autoplay_parser.add_argument(
        "--tap-duration", type=int, default=40,
        help="normal-note press duration in milliseconds (default: 40)",
    )
    live_autoplay_parser.add_argument(
        "--keys", help="comma-separated controls; mode-specific defaults cover standard layouts",
    )
    live_autoplay_parser.add_argument(
        "--playfield", help="desktop playfield rectangle x,y,width,height for osu!/catch",
    )
    live_autoplay_parser.add_argument(
        "--desktop", help="Linux virtual desktop rectangle x,y,width,height for uinput",
    )
    start_options = live_autoplay_parser.add_mutually_exclusive_group()
    start_options.add_argument(
        "--start-key", default="ENTER",
        help="key used to start the selected map (default: ENTER)",
    )
    start_options.add_argument(
        "--no-start-key", dest="start_key", action="store_const", const=None,
        help="wait for the user or game to start without emitting a start key",
    )
    live_autoplay_parser.add_argument(
        "--start-timeout", type=float, default=10.0,
        help="seconds to wait for a fresh gameplay timeline (default: 10)",
    )
    live_autoplay_parser.add_argument(
        "--jitter-ms", type=float, default=0.0,
        help="bounded seeded osu!mania timing jitter only (default: 0)",
    )
    live_autoplay_parser.add_argument(
        "--seed", type=int, default=0, help="seed for optional robustness jitter (default: 0)",
    )

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.command == "scan-maps":
            if not args.songs.is_dir():
                raise ValidationError(f"songs path is not a directory: {args.songs}")
            if not math.isfinite(args.min_ln_ratio) or args.min_ln_ratio < 0:
                raise ValidationError("min-ln-ratio must be a finite non-negative number")
            maps = scan_maps(args.songs, args.min_ln_ratio, args.limit)
            serializable = [dict(item, lnNormalRatio=("Infinity" if math.isinf(item["lnNormalRatio"]) else item["lnNormalRatio"])) for item in maps]
            print(json.dumps({"maps": serializable}, ensure_ascii=False, separators=(",", ":"), sort_keys=True, allow_nan=False))
        elif args.command == "capture":
            capture(args.base_url, args.label, args.out, args.timeout)
        elif args.command == "navigate":
            backend = create_keyboard_backend(
                args.backend,
                navigation_key_names(args.client, args.target, args.search),
            )
            navigate_ui(
                args.client,
                args.target,
                backend,
                args.search,
                args.key_delay,
                args.typing_delay,
                args.transition_delay,
            )
        elif args.command in ("autoplay", "live-autoplay"):
            plan = parse_play_plan(args.map, args.tap_duration)
            keys = key_mapping_for_plan(plan, args.keys)
            playfield = parse_rectangle(args.playfield, "--playfield")
            desktop = parse_rectangle(args.desktop, "--desktop")
            if args.command == "autoplay":
                backends = create_plan_backends(
                    plan, args.backend, keys, None, playfield, desktop
                )
                play_plan(plan, keys, backends, args.start_delay)
            else:
                start_key = (
                    normalize_key_name(args.start_key, "start key")
                    if args.start_key is not None
                    else None
                )
                backends = create_plan_backends(
                    plan, args.backend, keys, start_key, playfield, desktop
                )
                live_play_plan(
                    plan,
                    keys,
                    backends,
                    args.base_url,
                    start_key,
                    args.start_timeout,
                    args.jitter_ms,
                    args.seed,
                )
        return 0
    except (ValidationError, OSError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
