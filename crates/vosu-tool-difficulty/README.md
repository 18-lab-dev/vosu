# Difficulty

The first-party difficulty service calculates MinaCalc MSD, score-goal SSR and experimental dan estimates. Run the binary without arguments to serve the tool protocol; all CLI requests go through vosu to that service and its shared cache.

## Command line

`vosux difficulty [FILE.osu|-] [--rate R] [--score S] [--rates] [--json]` (also `vosu x difficulty …`). With no file, use the attached client's selected beatmap. `-` reads raw beatmap bytes from stdin; files and stdin are bounded to 1 MiB. Rate is 0.5–2.0 (the service rounds to 0.05); score is a Wife-style fraction 0–1, not osu! accuracy percentage. `--rates` prints the service's 0.7–2.0 rate table for the calculated checksum. Unsupported maps retain the service's explicit unsupported explanation.

Human output starts with the source, key count, note count and rate, followed by an aligned MSD/SSR skillset table (two decimals) and a dan estimate or an explanation of its absence. `--rates` prints one aligned table with rates as rows and skillsets as columns. `--json` emits the unchanged difficulty response, or `{checksum,rates}` with `--rates`, to stdout. Errors use `vosux difficulty: <service message> (HTTP … from …)` on stderr and exit 1. Terminal output uses no ANSI color. `--help` documents all arguments. Example: `cat chart.osu | vosux difficulty - --rate 0.85 --score 0.93 --json`.
