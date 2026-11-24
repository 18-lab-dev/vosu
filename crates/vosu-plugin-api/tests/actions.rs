//! F2: exact action declarations and discriminated wire contracts.
use vosu_plugin_api::{
    ActionInvoke, ActionSessionMode, Caller, CallerKind, CborValue, PluginManifest,
    ResourceResolved, decode, encode,
};

const BASE: &str = "kind='plugin'\nmanifest_version=1\nid='test.actions.plugin'\nname='Actions'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n";

fn declaration(name: &str, session: &str) -> String {
    format!(
        "\n[[actions]]\nname='{name}'\nversion='1.2.3'\ntitle='Run {name}'\nargs_schema='schemas/args.json'\nresult_schema='schemas/result.json'\nsession='{session}'\n"
    )
}

fn provide(name: &str) -> String {
    format!("\n[[provides]]\nid='action:test.actions.plugin.{name}'\nversion='1.2.3'\n")
}

fn fixture() -> String {
    format!(
        "{BASE}{}{}{}{}",
        declaration("first.run", "required"),
        declaration("second.run", "none"),
        provide("first.run"),
        provide("second.run")
    )
}

#[test]
fn two_actions_have_exact_names_modes_and_default_cli() {
    let manifest = PluginManifest::parse(&fixture()).unwrap();
    assert_eq!(manifest.actions.len(), 2);
    assert_eq!(manifest.actions[0].name, "first.run");
    assert_eq!(manifest.actions[1].name, "second.run");
    assert_eq!(manifest.actions[0].session, ActionSessionMode::Required);
    assert_eq!(manifest.actions[1].session, ActionSessionMode::None);
    assert!(!manifest.actions[0].cli);
    let optional = fixture().replace("session='none'", "session='optional'\ncli=true");
    let manifest = PluginManifest::parse(&optional).unwrap();
    assert_eq!(manifest.actions[1].session, ActionSessionMode::Optional);
    assert!(manifest.actions[1].cli);
}

#[test]
fn invalid_action_contracts_are_rejected() {
    let valid = fixture();
    for invalid in [
        format!("{valid}{}", declaration("first.run", "required")),
        format!("{valid}{}", provide("first.run")),
        format!("{BASE}{}", declaration("first.run", "required")),
        format!("{BASE}{}", provide("first.run")),
        valid.replace("name='first.run'", "name='First.run'"),
        valid.replace("version='1.2.3'", "version='not-semver'"),
        valid.replacen("version='1.2.3'", "version='2.0.0'", 1),
        valid.replace("title='Run first.run'", "title='  '"),
        valid.replace("schemas/args.json", "../outside.json"),
        valid.replace("schemas/result.json", "C:\\outside.json"),
        valid.replace("session='none'", "session='sometimes'"),
    ] {
        assert!(
            PluginManifest::parse(&invalid).is_err(),
            "accepted {invalid}"
        );
    }
}

#[test]
fn identical_arguments_preserve_two_distinct_action_ids() {
    #[derive(Default)]
    struct TwoActions(Vec<bool>);
    impl vosu_plugin_api::Plugin for TwoActions {
        fn create(
            _host: vosu_plugin_api::VtableHostServices,
            _init: vosu_plugin_api::Init,
        ) -> Result<Self, vosu_plugin_api::PluginError> {
            Ok(Self::default())
        }

        fn on_message(
            &mut self,
            message: vosu_plugin_api::Message,
            _correlation_id: u64,
        ) -> Result<(), vosu_plugin_api::PluginError> {
            let vosu_plugin_api::Message::ActionInvoke(call) = message else {
                return Err(vosu_plugin_api::vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
            };
            match call.action.as_str() {
                "test.actions.plugin.first.run" => self.0.push(true),
                "test.actions.plugin.second.run" => self.0.push(false),
                _ => return Err(vosu_plugin_api::vosu_status::VOSU_ERR_INVALID_ARGUMENT.into()),
            }
            Ok(())
        }
    }
    use vosu_plugin_api::{Message, Plugin, vosu_opcode};
    let invoke = |action: &str| ActionInvoke {
        invocation: 73,
        action: action.into(),
        session: Some(91),
        caller: Caller {
            kind: CallerKind::Plugin,
            id: "test.caller".into(),
        },
        args: CborValue::Integer(19.into()),
    };
    let first = invoke("test.actions.plugin.first.run");
    let second = invoke("test.actions.plugin.second.run");
    let a = encode(&first).unwrap();
    let b = encode(&second).unwrap();
    assert_ne!(a, b);
    assert_eq!(decode::<ActionInvoke>(&a).unwrap(), first);
    assert_eq!(decode::<ActionInvoke>(&b).unwrap(), second);
    let mut plugin = TwoActions::default();
    for payload in [&a, &b] {
        plugin
            .on_message(
                Message::decode(vosu_opcode::VOSU_OP_ACTION_INVOKE, 0, payload).unwrap(),
                0,
            )
            .unwrap();
    }
    assert_eq!(plugin.0, [true, false]);
    let mut wire: CborValue = decode(&a).unwrap();
    let CborValue::Map(fields) = &mut wire else {
        panic!("expected map")
    };
    fields.retain(|(key, _)| key.as_text() != Some("action"));
    assert!(decode::<ActionInvoke>(&encode(&wire).unwrap()).is_err());
}

#[test]
fn tree_lookup_preserves_canonical_index_entry_separate_from_storage_path() {
    let reply = ResourceResolved::TreePath {
        root: "store".into(),
        path: "hashes/1234".into(),
        entry_path: "skin/image.png".into(),
    };
    let bytes = encode(&reply).unwrap();
    assert_eq!(decode::<ResourceResolved>(&bytes).unwrap(), reply);
    let malformed = CborValue::Map(vec![
        (
            CborValue::Text("entry_path".into()),
            CborValue::Text("image.png".into()),
        ),
        (CborValue::Text("error".into()), CborValue::Null),
    ]);
    assert!(decode::<ResourceResolved>(&encode(&malformed).unwrap()).is_err());
}
