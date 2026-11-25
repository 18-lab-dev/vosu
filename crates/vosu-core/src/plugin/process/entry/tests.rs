use std::ffi::OsString;

use super::super::control::{EXIT_ARGUMENTS, EXIT_GENERATION};
use super::*;

const IDENTITY: &str = "vosu/test/plugin-host-1";

fn argv(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

#[cfg(target_os = "linux")]
fn valid() -> Vec<&'static str> {
    vec!["vosu", "plugin-host", IDENTITY, "4242", "3,4,5,6"]
}

#[cfg(not(target_os = "linux"))]
fn valid() -> Vec<&'static str> {
    vec!["vosu", "plugin-host", IDENTITY, "h2p=a4:a8,p2h=ac:b0"]
}

#[test]
fn only_the_exact_command_selects_the_plugin_host() {
    assert!(is_plugin_host(&argv(&["vosu", "plugin-host"])));
    for other in [
        vec!["vosu"],
        vec!["vosu", "plugin-host "],
        vec!["vosu", "Plugin-Host"],
        vec!["vosu", "--plugin-host"],
        vec!["vosu", "--setup", "plugin-host"],
        vec![],
    ] {
        assert!(!is_plugin_host(&argv(&other)), "{other:?}");
    }
}

#[test]
fn exact_arguments_parse() {
    let args = argv(&valid());
    let parsed = parse(&args, IDENTITY).unwrap();
    assert_eq!(parsed.endpoints, args.last().unwrap().as_os_str());
    #[cfg(target_os = "linux")]
    assert_eq!(parsed.parent, 4242);
}

#[test]
fn a_missing_extra_or_empty_argument_is_the_arguments_stage() {
    let base = valid();
    let mut cases = vec![
        base[..base.len() - 1].to_vec(),
        base[..2].to_vec(),
        [base.clone(), vec!["extra"]].concat(),
        [base.clone(), vec![base[base.len() - 1]]].concat(),
    ];
    let mut empty_identity = base.clone();
    empty_identity[2] = "";
    cases.push(empty_identity);
    let mut empty_endpoints = base.clone();
    *empty_endpoints.last_mut().unwrap() = "";
    cases.push(empty_endpoints);
    for case in cases {
        assert_eq!(
            parse(&argv(&case), IDENTITY),
            Err(EXIT_ARGUMENTS),
            "{case:?}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_noncanonical_parent_pid_is_the_arguments_stage() {
    for pid in [
        "0",
        "04242",
        "+4242",
        "-1",
        " 4242",
        "4242 ",
        "",
        "2147483648",
        "x",
    ] {
        let mut case = valid();
        case[3] = pid;
        assert_eq!(
            parse(&argv(&case), IDENTITY),
            Err(EXIT_ARGUMENTS),
            "{pid:?}"
        );
    }
    let mut largest = valid();
    largest[3] = "2147483647";
    assert!(parse(&argv(&largest), IDENTITY).is_ok());
}

#[test]
fn another_build_identity_is_the_generation_stage() {
    let args = argv(&valid());
    assert_eq!(
        parse(&args, "vosu/older/plugin-host-1"),
        Err(EXIT_GENERATION)
    );
    // A malformed command line is refused as such before the identity is
    // compared.
    let mut malformed = valid();
    malformed.push("extra");
    assert_eq!(
        parse(&argv(&malformed), "vosu/older/plugin-host-1"),
        Err(EXIT_ARGUMENTS)
    );
}

#[test]
fn run_refuses_before_touching_any_endpoint() {
    // Nothing named by these arguments is adopted or mapped: the refusal is
    // decided by the arguments alone.
    let mut case = valid();
    case.push("extra");
    assert_eq!(run(&argv(&case), IDENTITY), EXIT_ARGUMENTS);
    assert_eq!(run(&argv(&valid()), "another"), EXIT_GENERATION);
}
