# Privacy policy

Effective 2026-10-10. This policy covers the vosu daemon and its built-in dashboard as published from [shiruviaa/vosu](https://github.com/shiruviaa/vosu): the portable archives, Linux packages, and builds from this source.

vosu runs on your computer. It has no accounts, telemetry, analytics, advertising, or crash reporting, and the vosu maintainers run no servers that receive data from it. vosu still makes network requests to third-party services in the cases listed below, and every such service sees the usual connection metadata, including your IP address.

## What vosu reads on your computer

- **Game state** from the memory of a running osu! client: menus and game mode, the selected beatmap, gameplay statistics and inputs, player and leaderboard names, multiplayer and tournament details, and tournament chat where the client exposes it.
- **Game files** such as beatmaps, backgrounds, audio, and skin files, when a request to the local API asks for them.
- **Installed overlays and their settings** in the overlay folder and the configuration directory.

vosu does not send any of this to the vosu maintainers or to the services listed below. It publishes the data only through its local API.

## The local API and dashboard

vosu serves game state, game files, installed overlays, and the dashboard over plain HTTP and WebSocket. The listener is `SERVER_IP`:`SERVER_PORT`, `127.0.0.1:24050` by default, so by default only programs on this computer can connect.

- Programs on this computer that connect to the listener can read the game state. That includes browsers, OBS, overlays, and other local applications. Browser pages from other websites are rejected unless their `Origin` or `Referer` host is listed in `ALLOWED_IPS`.
- If you set a non-loopback `SERVER_IP` and allow other addresses in `ALLOWED_IPS`, devices on that network can read the same data and reach the dashboard. The connection is not encrypted and has no login. Never expose the listener to the internet.
- Installed overlays run their authors' JavaScript with the same access as the dashboard. See [Dashboard and counters](README.md#dashboard-and-counters).

## Network requests vosu makes

| Purpose | When | Destination | What is sent |
| --- | --- | --- | --- |
| Update check | On every start of a self-updating install (a portable archive or another user-writable install directory), whether or not `ENABLE_AUTOUPDATE` is on. Package-managed installs and debug builds never check. | `api.github.com` (GitHub) | An HTTP GET for the latest `shiruviaa/vosu` release, with the User-Agent `vosu/<version>` |
| Update download | When an update is installed: at startup if `ENABLE_AUTOUPDATE` is on, or when you click **Update Vosu** | GitHub release download hosts | An HTTP GET for the platform archive, with the same User-Agent |
| Overlay catalog | Each time you open **Available** or search on that page | `https://tosu.app/api.json`, operated by the tosu project | An HTTP GET with the User-Agent `vosu/<version> (+https://github.com/shiruviaa/vosu)` |
| Overlay download | When you click **Install** or **Update** on an available overlay | The download link that the catalog lists for that overlay. As of 2026-10-10 every listed link starts at `osuck.link` and redirects to `files.osuck.link`. | An HTTP GET for the overlay archive, with the same User-Agent as the catalog request |
| Catalog sources | When you open **Tools** or source overlays, refresh the catalog, or install/update a package | GitHub release download hosts for defaults `shiruviaa/vosu-tools`, `shiruviaa/vosulay`, and `shiruviaa/vosu-overlays`, plus configured source URLs | An HTTP GET with User-Agent `vosu-tool-catalog`; installed versions are compared locally |
| Official tool package | When you install or update a tool, including required dependencies | The HTTPS package URLs in the tool catalog | An HTTP GET with User-Agent `vosu-tool-catalog` |

Only the request itself is sent. vosu does not include game data, settings, installed-overlay lists, or other identifiers; it compares the catalog with your installed overlays locally. Host names are resolved through your system's DNS resolver. These requests connect directly and ignore system proxy settings. Catalog and overlay downloads must use HTTPS and may not redirect to loopback, private, or reserved addresses.

## Requests your browser makes from the dashboard

- **Catalog preview images** on the **Available** page load directly from the image URLs listed in the catalog, which as of 2026-10-10 are all on `raw.githubusercontent.com` (GitHub). Your browser sends its normal request headers, including your IP address and User-Agent. The dashboard asks it not to send a `Referer`, and only the previews that scroll into view are loaded.
- **Links** such as the source repository, author pages, the upstream catalog, and this policy open only when you click them.
- **Installed overlays** are third-party web pages. Their authors decide whether they load fonts, scripts, or images from the internet or send data elsewhere, and they can read everything the local API exposes. vosu does not review or control overlays.

## Data stored on your computer

[Files and migration](README.md#files-and-migration) lists the platform paths.

- Configuration in `vosu.toml`, and overlay setting values under `settings/`.
- Installed overlays in the overlay folder (`STATIC_FOLDER_PATH`).
- Official and user tools under `tools/<id>/` in the data directory, tool trust/settings state under the config directory's `tools/`, and per-tool durable data and cache folders. Uninstalling an official tool requires choosing whether to keep or remove its data.
- Caches: lazer memory offsets, the extracted offset-generator payload, and partial downloads.
- Logs under `logs/` in the data directory. They are rotated, and the oldest files are removed once the folder exceeds 100 MiB.
- While an update check or installation runs, a transaction record (`.vosu-update.json`) and staging files next to a self-updating executable.
- On Linux, the systemd user unit created when **Start at login** is enabled.
- In your browser's local storage for the dashboard's address: the order of pinned overlays and the last installed and available overlay counts.

**Download report** builds a diagnostic report on demand and hands it to your browser as a download. It contains machine details, configuration, the installed-overlay list, and recent logs. vosu never uploads it. Review it before you share it with anyone.

## Third-party services

- **GitHub** hosts vosu releases and the catalog's preview images. See the [GitHub General Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).
- **tosu.app** is the tosu project's catalog service. vosu does not operate or control it. Its handling of requests is governed by the tosu project.
- **osuck.link** and **files.osuck.link** are operated by cyperdark, a tosu contributor, and serve the overlay archives that the catalog currently links to. vosu does not operate or control them.
- If the catalog later lists other download or image hosts, those hosts receive the requests described above instead.

## Limiting network use

- Package-managed Linux installs never check for or download updates.
- On self-updating installs, `ENABLE_AUTOUPDATE = false` or `--update=false` stops vosu from installing updates. The startup check still runs and only reports when a new version is available. Block `api.github.com` for vosu in your firewall if you want no update traffic at all.
- vosu contacts overlay catalog hosts when you use **Available** or install/update an overlay. Official tool catalog requests happen when you use **Tools**, refresh its catalog, or install/update a tool; packages download only on install/update.

## Changes and contact

Changes to this policy are recorded in this file's history in the repository. For questions, open an issue at <https://github.com/shiruviaa/vosu/issues>.
