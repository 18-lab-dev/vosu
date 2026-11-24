//! Fixture gate counterexamples: a temporary copy of the synthetic fixture
//! is edited the way a bad candidate or a hand edit would be, then loaded.
//! `reseal` recomputes the inventories, so the later gates are reached
//! instead of the hash check.
use super::{
    Value,
    case_file::{self, excluded_target},
    envelope::selection_digest,
    fixture::{self, Fixture, Pinned, Provenance},
    golden_cases::{pin, synthetic_dir},
    json,
};
use std::path::Path;
use tempfile::TempDir;

const PLATFORM: &str = "windows-stable";

fn copy_tree(from: &Path, to: &Path) {
    for file in fixture::walk(from).unwrap() {
        let target = to.join(&file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(from.join(&file), target).unwrap();
    }
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn write(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn edit_json(path: &Path, edit: impl FnOnce(&mut Value)) {
    let mut value = read(path);
    edit(&mut value);
    write(path, &value);
}

/// Recomputes every platform inventory, then the revision inventory.
fn reseal(root: &Path) {
    let entries = |dir: &Path| -> Vec<Value> {
        fixture::walk(dir)
            .unwrap()
            .into_iter()
            .filter(|file| file != "inventory.json")
            .map(|file| {
                let bytes = std::fs::read(dir.join(&file)).unwrap();
                json!({"path": file, "bytes": bytes.len(), "sha256": fixture::sha256(&bytes)})
            })
            .collect()
    };
    for name in ["linux-lazer", PLATFORM] {
        let platform = root.join(name);
        let files = entries(&platform);
        edit_json(&platform.join("inventory.json"), |v| {
            v["files"] = json!(files)
        });
    }
    let files = entries(root);
    edit_json(&root.join("inventory.json"), |v| v["files"] = json!(files));
}

fn copy() -> TempDir {
    let dir = TempDir::new().unwrap();
    copy_tree(&synthetic_dir(), dir.path());
    dir
}

fn load(root: &Path, provenance: Provenance) -> Result<Fixture, String> {
    fixture::load(root, &pin(), provenance)
}

fn refused(edit: impl FnOnce(&Path), seal: bool, message: &str) {
    let dir = copy();
    edit(dir.path());
    if seal {
        reseal(dir.path());
    }
    let error = load(dir.path(), Provenance::Synthetic)
        .err()
        .unwrap_or_else(|| panic!("a fixture tree expected to fail with {message:?} was accepted"));
    assert!(error.contains(message), "{message:?} not in {error:?}");
}

fn case_path(root: &Path, id: &str) -> std::path::PathBuf {
    root.join(PLATFORM).join("cases").join(format!("{id}.json"))
}

#[test]
fn golden_gates_accept_the_untouched_synthetic_copy() {
    let dir = copy();
    reseal(dir.path());
    let fixture = load(dir.path(), Provenance::Synthetic).unwrap();
    let names: Vec<_> = fixture.platforms.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["linux-lazer", PLATFORM]);
    assert!(fixture.rules.is_none());
}

#[test]
fn golden_gates_refuse_tampered_or_incomplete_trees() {
    let v2 = "state-http.get-json-v2";
    refused(
        |root| {
            let path = case_path(root, v2);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.push(b'\n');
            std::fs::write(path, bytes).unwrap();
        },
        false,
        "differs from its inventory hash",
    );
    refused(
        |root| {
            std::fs::copy(case_path(root, v2), case_path(root, "state-http.extra")).unwrap();
        },
        false,
        "is not inventoried",
    );
    refused(
        |root| {
            std::fs::copy(case_path(root, v2), case_path(root, "state-http.extra")).unwrap();
        },
        true,
        "listed cases differ",
    );
    refused(
        |root| std::fs::remove_file(case_path(root, v2)).unwrap(),
        false,
        "not a fixture file",
    );
    refused(
        |root| std::fs::write(root.join(PLATFORM).join("notes.txt"), "x").unwrap(),
        false,
        "unknown fixture file",
    );
    refused(
        |root| std::fs::write(root.join("extra.json"), "{}").unwrap(),
        false,
        "unknown fixture file",
    );
    refused(
        |root| {
            let big = vec![b' '; usize::try_from(fixture::JSON_BUDGET).unwrap()];
            std::fs::write(case_path(root, "state-http.zz"), big).unwrap();
        },
        false,
        "over budget",
    );
    // The brand inventory must list exactly the files' old-brand counts.
    refused(
        |root| {
            let path = root.join(PLATFORM).join("brand-inventory.txt");
            let mut text = std::fs::read_to_string(&path).unwrap();
            text.push_str(&format!(
                "tests/fixtures/upstream/{}/{PLATFORM}/cases/{v2}.json 1 parity upstream wire\n",
                pin()
            ));
            std::fs::write(path, text).unwrap();
        },
        true,
        "old-brand counts",
    );
}

#[test]
fn golden_gates_refuse_unapproved_content() {
    let calc = "calculation.no-query";
    refused(
        |root| {
            edit_json(&case_path(root, calc), |v| {
                v["request"]["text"] = json!("GET /api/ingame HTTP/1.1\r\nHost: x\r\n\r\n");
            });
        },
        true,
        "/api/ingame",
    );
    // A zero-case family is not evidence.
    refused(
        |root| {
            std::fs::remove_file(case_path(root, calc)).unwrap();
            edit_json(&root.join(PLATFORM).join("manifest.json"), |v| {
                v["cases"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|c| c["id"] != calc);
                v["case_counts"]["calculation"] = json!(0);
            });
        },
        true,
        "zero calculation cases",
    );
    refused(
        |root| {
            edit_json(&case_path(root, calc), |v| {
                v["normalized"] = json!(["client-timestamp-pointer", "header-date"]);
            });
        },
        true,
        "no approved instance",
    );
    refused(
        |root| {
            edit_json(&case_path(root, calc), |v| {
                let headers = v["expected"]["headers"].as_array_mut().unwrap();
                let date = headers.iter_mut().find(|h| h[0] == "date").unwrap();
                date[1] = json!("Mon, 24 Nov 2026 10:00:00 GMT");
            });
        },
        true,
        "is not normalized by its rule",
    );
    refused(
        |root| {
            edit_json(&case_path(root, calc), |v| {
                v["expected"]["frames"] = json!([])
            });
        },
        true,
        "members",
    );
    refused(
        |root| {
            edit_json(&case_path(root, calc), |v| {
                v["request"]["text"] = json!("GET /x HTTP/1.1\r\nX-Note: <golden:date>\r\n\r\n");
            });
        },
        true,
        "unapproved placeholder",
    );
    for spelling in [
        "GET /API/InGame HTTP/1.1",
        "GET //api//ingame HTTP/1.1",
        "GET /api/%69ngame HTTP/1.1",
        "GET /api/%2569ngame HTTP/1.1",
        "GET \\api\\ingame HTTP/1.1",
        "GET /__vosu_test/v1/adapter-capture HTTP/1.1",
    ] {
        assert!(excluded_target(spelling).is_err(), "{spelling}");
    }
    assert!(excluded_target("GET /api/calculate/pp HTTP/1.1").is_ok());
}

/// Relabels the synthetic copy as a capture, as a regenerated-from-vosu
/// fixture would be.
fn relabel(root: &Path, edit: impl Fn(&mut Value)) {
    edit_json(&root.join("manifest.json"), |v| {
        v["provenance"] = json!("captured")
    });
    for name in ["linux-lazer", PLATFORM] {
        edit_json(&root.join(name).join("manifest.json"), |v| {
            v["provenance"] = json!("captured");
            edit(v);
        });
    }
    reseal(root);
}

#[test]
fn golden_gates_keep_synthetic_fixtures_out_of_evidence() {
    // A synthetic tree never loads as a capture...
    let error = load(&synthetic_dir(), Provenance::Captured).unwrap_err();
    assert!(error.contains("never pilot or full evidence"), "{error}");
    // ...nor at the pin directory.
    let upstream = TempDir::new().unwrap();
    copy_tree(&synthetic_dir(), &upstream.path().join(pin()));
    let error = fixture::discover(upstream.path(), &pin()).err().unwrap();
    assert!(error.contains("never pilot or full evidence"), "{error}");
    // A capture needs main's review receipt.
    let dir = copy();
    relabel(dir.path(), |_| {});
    let error = load(dir.path(), Provenance::Captured).unwrap_err();
    assert!(error.contains("review receipt"), "{error}");
    let error = load(dir.path(), Provenance::Synthetic).unwrap_err();
    assert!(error.contains("expected a synthetic"), "{error}");
    // Expected values attested as the vosu binary are vosu output.
    let dir = copy();
    relabel(dir.path(), |v| {
        v["review_receipt"] = json!("main-review-1");
        v["source"]["sha256"] = v["vosu"]["sha256"].clone();
    });
    let error = load(dir.path(), Provenance::Captured).unwrap_err();
    assert!(error.contains("only expected source"), "{error}");
    // Another pin's tree is not this pin's fixture.
    let error =
        fixture::load(&synthetic_dir(), &"0".repeat(40), Provenance::Synthetic).unwrap_err();
    assert!(error.contains("not the Cargo pin"), "{error}");
}

#[test]
fn golden_gates_require_exact_allowlist_lines_for_committed_fixtures() {
    let dir = TempDir::new().unwrap();
    let old = ["to", "su"].concat();
    std::fs::write(dir.path().join("body.json"), format!("\"{old}\"")).unwrap();
    let line = format!(
        "tests/fixtures/upstream/{}/{PLATFORM}/body.json 1 parity upstream wire observation",
        pin()
    );
    std::fs::write(
        dir.path().join("brand-inventory.txt"),
        format!("# header\n{line}\n"),
    )
    .unwrap();
    let files = fixture::walk(dir.path()).unwrap();
    // A synthetic tree only has to count; a committed one needs the exact
    // allowlist entry, which is added only in the reviewed fixture commit.
    fixture::brand(dir.path(), PLATFORM, &pin(), Provenance::Synthetic, &files).unwrap();
    let error =
        fixture::brand(dir.path(), PLATFORM, &pin(), Provenance::Captured, &files).unwrap_err();
    assert!(
        error.contains("not an exact scripts/brand-allowlist.txt entry"),
        "{error}"
    );
    std::fs::write(dir.path().join("brand-inventory.txt"), "# header\n").unwrap();
    let error =
        fixture::brand(dir.path(), PLATFORM, &pin(), Provenance::Synthetic, &files).unwrap_err();
    assert!(error.contains("old-brand counts"), "{error}");
}

#[test]
fn golden_pin_selection_applies_the_pending_marker_rules() {
    let pin = pin();
    let other = "0".repeat(40);
    let dir = TempDir::new().unwrap();
    let upstream = dir.path();
    let refused = |message: &str| {
        let error = fixture::discover(upstream, &pin).err().unwrap();
        assert!(error.contains(message), "{message:?} not in {error:?}");
    };
    refused(&format!("no golden fixtures for pin {pin}"));
    let marker = upstream.join(fixture::MARKER);
    std::fs::write(&marker, format!("{other}\nwaiting\n")).unwrap();
    refused("names");
    std::fs::write(&marker, format!("{pin}\n")).unwrap();
    refused("one-line reason");
    std::fs::write(&marker, format!("{pin}\nwaiting\nmore\n")).unwrap();
    refused("one-line reason");
    std::fs::write(&marker, format!("{pin}\nwaiting for O0\n")).unwrap();
    assert!(matches!(
        fixture::discover(upstream, &pin),
        Ok(Pinned::Pending { reason }) if reason == "waiting for O0"
    ));
    // An old pin's directory fails even beside a valid marker.
    std::fs::create_dir(upstream.join(&other)).unwrap();
    refused("a pin move deletes");
    std::fs::remove_dir(upstream.join(&other)).unwrap();
    std::fs::create_dir(upstream.join(&pin)).unwrap();
    refused("beside the golden fixtures");
    // The three pins agree.
    assert_eq!(fixture::agree(&pin, &pin).unwrap(), pin);
    assert!(
        fixture::agree(&pin, &other)
            .unwrap_err()
            .contains("differs")
    );
    assert!(fixture::agree("main", "main").is_err());
}

/// A platform's production host decides the foreground facts it records:
/// Windows always has a source (`true`/`false`), Linux never does (`null`).
/// A `null` on a Windows fixture, or a fact on a Linux one, was not
/// recorded by that host and is refused rather than replayed.
#[test]
fn golden_gates_refuse_foreground_facts_the_platform_never_records() {
    let v2 = "state-http.get-json-v2";
    let set = |platform: &'static str, foreground: Value| {
        move |root: &Path| {
            let path = root.join(platform).join("cases").join(format!("{v2}.json"));
            edit_json(&path, |case| {
                let replay = &mut case["replay"];
                replay["session"]["foreground"] = foreground.clone();
                replay["roster"]["sessions"][0]["foreground"] = foreground.clone();
                let digest = selection_digest(replay).unwrap();
                replay["digest"] = json!(digest);
            });
        }
    };
    refused(set(PLATFORM, Value::Null), true, "never reports");
    refused(set("linux-lazer", json!(true)), true, "never reports");
    refused(set("linux-lazer", json!(false)), true, "never reports");
    // The facts each platform actually records load.
    for (platform, foreground) in [(PLATFORM, json!(true)), ("linux-lazer", Value::Null)] {
        let dir = copy();
        set(platform, foreground)(dir.path());
        reseal(dir.path());
        load(dir.path(), Provenance::Synthetic).unwrap_or_else(|e| panic!("{platform}: {e}"));
    }
}

#[test]
fn golden_case_files_take_the_closed_outcome_and_refuse_non_golden_roles() {
    let dir = copy();
    let platform = dir.path().join(PLATFORM);
    let v2 = "state-http.get-json-v2";
    // An upstream connection closed without a response is an explicit outcome.
    let closed = "state-http.closed";
    let mut value = read(&case_path(dir.path(), v2));
    value["id"] = json!(closed);
    value["expected"] = json!({"outcome": "closed"});
    write(&case_path(dir.path(), closed), &value);
    let case = case_file::case(&platform, PLATFORM, closed, &[]).unwrap();
    assert!(case.expected.closed);
    for (expected, message) in [
        (json!({"outcome": "reset"}), "not closed"),
        (json!({"outcome": "closed", "status": 200}), "expected"),
    ] {
        value["expected"] = expected;
        write(&case_path(dir.path(), closed), &value);
        let error = case_file::case(&platform, PLATFORM, closed, &[])
            .err()
            .unwrap();
        assert!(error.contains(message), "{message:?} not in {error:?}");
    }
    // Negative and diagnostic route cases are never goldens.
    for id in ["state-http.negative.api-v1-suffix", "ws.v2.diagnostic.feed"] {
        let mut value = read(&case_path(dir.path(), v2));
        value["id"] = json!(id);
        value["family"] = json!(id.split('.').next().unwrap());
        write(&case_path(dir.path(), id), &value);
        let error = case_file::case(&platform, PLATFORM, id, &[]).err().unwrap();
        assert!(error.contains("never goldens"), "{error}");
    }
}
