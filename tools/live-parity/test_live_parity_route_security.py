from __future__ import annotations

import dataclasses
import os
from pathlib import Path
import sys
import tempfile
import unittest
from urllib.parse import unquote

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import parity_routes as pr
import route_matrix as rm
import route_security as rs
from test_live_parity_routes import CONTEXT_VALUES


class SecurityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name, "capture")
        self.songs = self.root / "game" / "Songs"
        self.skins = self.root / "game" / "Skins" / "Probe"
        (self.songs / "Probe Set").mkdir(parents=True)
        self.skins.mkdir(parents=True)
        (self.songs / "Probe Set" / "map.osu").write_text("osu file format v14\n")
        (self.skins / "cursor.png").write_bytes(b"png")
        self.layout = rs.prepare_security_layout(
            str(self.root), {"/Songs/": str(self.songs), "/files/beatmap/": str(self.songs),
                             "/files/skin/": str(self.skins)},
            {"/Songs/": "Probe Set/map.osu", "/files/beatmap/": "Probe Set/map.osu",
             "/files/skin/": "cursor.png"},
            chart_source=str(self.songs / "Probe Set" / "map.osu"),
            other_roots={"files": str(self.songs)})
        values = dict(CONTEXT_VALUES, calc_absolute_inside=str(self.songs / "Probe Set" / "map.osu"))
        self.context = rm.MatrixContext(f"{rs.host_os()}-lazer", values, self.layout)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_every_native_spelling_names_only_the_run_owned_sentinel(self) -> None:
        if rs.host_os() != "linux":
            self.skipTest("POSIX spellings")
        selected, excluded = rm.select(
            [case for case in rm.MATRIX if case.family == "path-security"], self.context)
        names = {item.case.security[1] for item in selected}
        self.assertTrue({"encoded-parent", "double-encoded-parent", "backslash-encoded",
                         "absolute-encoded", "absolute-double-slash"} <= names)
        if self.layout.links:
            self.assertIn("symlink", names)
        sentinel = os.path.realpath(self.layout.sentinel)
        for item in selected:
            self.assertFalse(item.lexical_only)
            target = item.request.split(b" ")[1].decode()
            prefix = item.case.security[0]
            spelled = target[len(prefix):]
            decoded = unquote(unquote(spelled)).replace("\\", "/")
            path = decoded if decoded.startswith("/") else os.path.join(
                self.layout.route_roots[prefix], decoded)
            self.assertEqual(sentinel, os.path.realpath(path), item.case.id)
        self.assertTrue(all("requires windows" in reason for _, reason in excluded
                            if "symlink" not in reason))

    def test_aborts_before_sending_for_any_target_outside_the_capture_root(self) -> None:
        sent = []
        home_file = str(Path.home() / ".profile")
        bad = {
            "home sentinel": dataclasses.replace(self.layout, sentinel=home_file),
            "home capture root": dataclasses.replace(self.layout, capture_root=str(Path.home())),
            "root outside capture": dataclasses.replace(
                self.layout, route_roots={"/Songs/": str(Path(self.temp.name, "elsewhere"))}),
            "foreign marker": dataclasses.replace(self.layout, token="0" * 32),
        }
        case = rm.CASES["path-security.songs.encoded-parent"]
        for reason, layout in bad.items():
            with self.subTest(reason=reason):
                context = dataclasses.replace(self.context, security=layout)
                with self.assertRaises(rs.SecurityAbort):
                    item = rm.render(case, context)
                    sent.append(item)
        Path(self.layout.sentinel).write_text("not ours\n")
        with self.assertRaises(rs.SecurityAbort):
            rm.render(case, self.context)
        self.assertEqual([], sent)

    def test_retargeted_symlink_and_outside_calculation_paths_abort(self) -> None:
        if "/Songs/" not in self.layout.links:
            self.skipTest("symlinks unavailable")
        link = self.songs / rs.LINK_NAME
        other = self.root / "other.txt"
        other.write_text("x")
        link.unlink()
        link.symlink_to(other)
        with self.assertRaises(rs.SecurityAbort):
            rm.render(rm.CASES["path-security.songs.symlink"], self.context)
        outside = dataclasses.replace(self.layout, sentinel_chart=str(Path.home() / "x.osu"))
        with self.assertRaises(rs.SecurityAbort):
            rm.render(rm.CASES["calculation.absolute-outside-root"],
                      dataclasses.replace(self.context, security=outside))
        values = dict(self.context.values, calc_absolute_inside=self.layout.sentinel)
        with self.assertRaises(rs.SecurityAbort):
            rm.render(rm.CASES["calculation.absolute-inside-root-osu"],
                      dataclasses.replace(self.context, values=values))
        Path(self.layout.sentinel_chart).write_text("changed")
        with self.assertRaises(rs.SecurityAbort):
            rm.render(rm.CASES["calculation.absolute-outside-root"], self.context)

    def test_non_route_declared_root_admits_inside_paths_and_bounds_sentinels(self) -> None:
        files = self.root / "lazer" / "files"
        files.mkdir(parents=True)
        chart = files / "a" / "probe.osu"
        chart.parent.mkdir()
        chart.write_text("osu file format v14\n")
        layout = dataclasses.replace(self.layout, other_roots={"files": str(files)})
        values = dict(self.context.values, calc_relative="a/probe.osu", calc_absolute_inside=str(chart))
        context = rm.MatrixContext(self.context.platform, values, layout)
        case = rm.CASES["calculation.absolute-inside-root-osu"]
        rm.render(case, context)
        with self.assertRaises(rs.SecurityAbort):
            rm.render(case, dataclasses.replace(
                context, values=dict(context.values, calc_absolute_inside=str(Path.home()))))
        swallowed = dataclasses.replace(
            layout, other_roots={"files": str(self.root / "vosu-route-probe")})
        with self.assertRaisesRegex(rs.SecurityAbort, "inside a declared root"):
            rm.render(rm.CASES["calculation.absolute-outside-root"],
                      dataclasses.replace(context, security=swallowed))

    def test_calculation_paths_are_validated_and_query_encoded(self) -> None:
        outside = rm.render(rm.CASES["calculation.absolute-outside-root"], self.context)
        inside = rm.render(rm.CASES["calculation.absolute-inside-root-osu"], self.context)
        relative = rm.render(rm.CASES["calculation.relative-osu-path"], self.context)
        for item, path in ((outside, self.layout.sentinel_chart),
                           (inside, self.context.values["calc_absolute_inside"]),
                           (relative, self.context.values["calc_relative"])):
            target = item.request.split(b" ")[1].decode()
            self.assertEqual(path, unquote(target.split("?path=", 1)[1]))
            self.assertNotIn(" ", target)
        self.assertNotIn("legacy-calculate-declared-roots", inside.case.ledger)
        self.assertNotIn("legacy-calculate-declared-roots", relative.case.ledger)
        self.assertIn("legacy-calculate-declared-roots", outside.case.ledger)
        stable = dataclasses.replace(self.context, platform="windows-stable")
        self.assertEqual(inside.request, rm.render(inside.case, stable).request)

    def test_both_calculation_probes_refuse_hashes_and_mismatched_spellings(self) -> None:
        chart = self.songs / "hash"
        chart.write_bytes(b"osu file format v14\n")
        for case in ("calculation.relative-osu-path", "calculation.absolute-inside-root-osu"):
            for relative, absolute, error in (
                    ("hash", chart, r"ordinary relative \.osu file"),
                    ("Probe Set/map.osu", chart, "must name the same file")):
                with self.subTest(case=case, relative=relative), \
                        self.assertRaisesRegex(rs.SecurityAbort, error):
                    rm.render(rm.CASES[case], dataclasses.replace(
                        self.context, values=dict(self.context.values, calc_relative=relative,
                                                  calc_absolute_inside=str(absolute))))

    def test_windows_spellings_are_lexical_only_and_never_sent_from_linux(self) -> None:
        if rs.host_os() == "windows":
            self.skipTest("foreign-platform check")
        layout = rs.SecurityLayout(
            "windows", r"C:\vosu-capture\run1", "t" * 32,
            {"/Songs/": r"C:\vosu-capture\run1\osu!\Songs"},
            {"/Songs/": "Probe Set/map.osu"}, r"C:\vosu-capture\run1\vosu-route-probe\sentinel.txt")
        context = rm.MatrixContext("windows-stable", CONTEXT_VALUES, layout)
        spelled = {}
        for spelling in ("encoded-parent", "absolute-drive", "absolute-drive-slash",
                         "unc-extended", "unc-device", "ads", "trailing-dot", "trailing-space",
                         "backslash-raw"):
            item = rm.render(rm.CASES[f"path-security.songs.{spelling}"], context)
            self.assertTrue(item.lexical_only)
            spelled[spelling] = item.request.split(b" ")[1].decode()
            with self.assertRaises(rs.SecurityAbort):
                pr.run_case(item, pr.RouteEndpoint("source", 1))
        self.assertEqual("/Songs/..%2F..%2Fvosu-route-probe%2Fsentinel.txt",
                         spelled["encoded-parent"])
        self.assertTrue(spelled["ads"].endswith("map.osu::$DATA"))
        self.assertTrue(spelled["trailing-dot"].endswith("map.osu."))
        self.assertTrue(spelled["trailing-space"].endswith("map.osu%20"))
        self.assertTrue(spelled["unc-extended"].startswith("/Songs/%5C%5C%3F%5CC%3A"))
        self.assertTrue(spelled["absolute-drive-slash"].startswith("/Songs/C:/vosu-capture/"))
        for bad in (dataclasses.replace(layout, sentinel=r"C:\Users\vosu\secret.txt"),
                    dataclasses.replace(layout, capture_root=r"C:\Users\vosu"),
                    dataclasses.replace(layout, capture_root="C:\\")):
            with self.assertRaises(rs.SecurityAbort):
                rm.render(rm.CASES["path-security.songs.absolute-drive"],
                          dataclasses.replace(context, security=bad))

    def test_prepare_never_adopts_existing_files_and_cleanup_is_exact(self) -> None:
        with self.assertRaises(FileExistsError):
            rs.prepare_security_layout(str(self.root), {"/Songs/": str(self.songs)},
                                       {"/Songs/": "Probe Set/map.osu"})
        unrelated = self.songs / "Probe Set" / "map.osu"
        self.assertEqual(self.layout.upstream_static, str(self.root / rs.UPSTREAM_STATIC_DIR))
        self.assertTrue(Path(self.layout.upstream_static).is_dir())
        Path(self.layout.upstream_static, "written-by-upstream").write_text("x")
        rs.remove_security_layout(self.layout)
        self.assertFalse(Path(self.layout.sentinel).exists())
        self.assertFalse((self.root / rs.MARKER_NAME).exists())
        self.assertFalse((self.songs / rs.LINK_NAME).is_symlink())
        self.assertFalse(Path(self.layout.upstream_static).exists())
        self.assertTrue(unrelated.is_file())

    def test_sentinel_exposure_detection(self) -> None:
        self.assertTrue(rs.sentinel_exposed(
            rs.sentinel_content(self.layout.token).encode(), self.layout))
        self.assertFalse(rs.sentinel_exposed(b"Not Found", self.layout))
        self.assertEqual(self.layout.token, Path(self.root, rs.MARKER_NAME).read_text())


if __name__ == "__main__":
    unittest.main()
