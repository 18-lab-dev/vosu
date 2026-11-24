use super::*;
#[test]
fn stable_manager_priority_aggregate_all_sessions_and_generation_replacement() {
    let harness=Harness::new("vosu.osu",0x6a00_1100,4);
    let [manager,ordinary,right,left]=harness.pids[..].try_into().unwrap();
    for (pid,args) in [(manager,vec!["--tournament"]),(ordinary,vec![]),(right,vec!["--spectateclient=11"]),(left,vec!["--spectateclient=3"])] {
        harness.host.process_appeared(Harness::record(pid,&args));
    }
    eventually(|| HANDLES.lock().unwrap().len()==4);
    let handles=HANDLES.lock().unwrap().clone();
    eventually(|| namespace(&harness,handles[&ordinary],"vosu.osu").is_some_and(|v| v["tourney"]["clients"].as_array().is_some_and(|v| v.len()==2)));
    eventually(|| {
        let aggregate=namespace(&harness,handles[&ordinary],"vosu.osu").unwrap()["tourney"].clone();
        [manager,ordinary,right,left].iter().all(|pid| namespace(&harness,handles[pid],"vosu.osu").is_some_and(|v|v["tourney"]==aggregate))
    });
    let aggregate=namespace(&harness,handles[&ordinary],"vosu.osu").unwrap()["tourney"].clone();
    assert_eq!(aggregate["clients"][0]["ipcId"],3.0);
    assert_eq!(aggregate["clients"][1]["ipcId"],11.0);
    assert_eq!(aggregate["clients"][0]["team"],"left");
    assert_eq!(aggregate["clients"][1]["team"],"right");
    harness.foreground.set(Some(ordinary));
    harness.host.foreground_changed();
    eventually(|| namespace(&harness,handles[&manager],"vosu.client").is_some_and(|v|v["focused"]==true));
    assert_eq!(namespace(&harness,handles[&ordinary],"vosu.client").unwrap()["focused"],false,"priority is not the plain OS foreground rule");
    #[cfg(windows)] {
        eventually(|| namespace(&harness,handles[&ordinary],"vosu.osu").is_some_and(|v|v["session"]["focused"]==true));
        assert_eq!(namespace(&harness,handles[&manager],"vosu.osu").unwrap()["session"]["focused"],false,"per-session foreground facts remain accurate for adapter-owned OR");
    }
    harness.host.process_exited(left);
    eventually(|| namespace(&harness,handles[&ordinary],"vosu.osu").is_some_and(|v|v["tourney"]["clients"].as_array().is_some_and(|v|v.len()==1)));
    harness.host.process_exited(manager);
    eventually(|| namespace(&harness,handles[&ordinary],"vosu.osu").is_some_and(|v|v["tourney"]["clients"].as_array().is_some_and(Vec::is_empty)));
    harness.host.process_appeared(Harness::record(manager,&["--tournament"]));
    eventually(|| HANDLES.lock().unwrap()[&manager]!=handles[&manager]);
    let replacement=HANDLES.lock().unwrap()[&manager];
    eventually(|| namespace(&harness,replacement,"vosu.client").is_some_and(|v|v["focused"]==true));
    eventually(|| namespace(&harness,handles[&ordinary],"vosu.osu").is_some_and(|v|v["tourney"]["clients"].as_array().is_some_and(|v|v.len()==1)));
    assert_dispatches_ok();
}
